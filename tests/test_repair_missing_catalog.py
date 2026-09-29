import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('repair_catalog', Path(__file__).resolve().parents[1] / 'distro/tools/repair-missing-catalog.py')
repair = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repair)


class RepairCatalogTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.media = self.root / 'dados/musicas'
        self.media.mkdir(parents=True)
        self.db = self.root / 'dados/jukebox.db'
        with sqlite3.connect(self.db) as db:
            db.executescript('CREATE TABLE tracks(file_path TEXT PRIMARY KEY);'
                             'CREATE TABLE media_fingerprints(file_path TEXT PRIMARY KEY, signature TEXT);'
                             'CREATE TABLE system_state(key TEXT,value INTEGER);'
                             'CREATE TABLE playback_queue(track_json TEXT);'
                             "INSERT INTO system_state VALUES('credits',42);"
                             "INSERT INTO playback_queue VALUES('keep queued track');")

    def add(self, path):
        with sqlite3.connect(self.db) as db:
            db.execute('INSERT INTO tracks VALUES(?)', (str(path),))
            db.execute("INSERT INTO media_fingerprints VALUES(?,'signature')", (str(path),))

    def test_only_missing_catalog_paths_removed_with_complete_backup(self):
        missing = './dados/musicas/Rock/Artista/Disco/01.mp3'
        existing = self.media / 'Gospel/Artista/Disco/01.mp3'
        existing.parent.mkdir(parents=True)
        existing.write_bytes(b'audio')
        outside = self.root / 'outside/missing.mp3'
        for path in (missing, existing, outside):
            self.add(path)
        count, backup = repair.repair(self.db, self.media)
        self.assertEqual(count, 1)
        with sqlite3.connect(self.db) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM tracks').fetchone()[0], 2)
            self.assertEqual(db.execute('SELECT count(*) FROM media_fingerprints').fetchone()[0], 2)
            self.assertEqual(db.execute('SELECT value FROM system_state').fetchone()[0], 42)
            self.assertEqual(db.execute('SELECT track_json FROM playback_queue').fetchone()[0], 'keep queued track')
        with sqlite3.connect(backup) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM tracks').fetchone()[0], 3)
        self.assertEqual(repair.repair(self.db, self.media), (0, None))

    def test_unavailable_root_does_not_clear_catalog(self):
        self.add(self.media / 'missing.mp3')
        self.media.rmdir()
        with self.assertRaises(ValueError):
            repair.repair(self.db, self.media)
        with sqlite3.connect(self.db) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM tracks').fetchone()[0], 1)

    def test_access_error_preserves_entry(self):
        inaccessible = self.media / 'inaccessible.mp3'
        self.add(inaccessible)
        original = Path.stat
        def stat(path, *args, **kwargs):
            if path == inaccessible:
                raise PermissionError('denied')
            return original(path, *args, **kwargs)
        with patch.object(Path, 'stat', stat):
            self.assertEqual(repair.repair(self.db, self.media), (0, None))

    def test_wrong_database_path_is_not_created(self):
        missing = self.root / 'wrong.db'
        with self.assertRaises(ValueError):
            repair.repair(missing, self.media)
        self.assertFalse(missing.exists())
