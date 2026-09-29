import importlib.util
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'distro/config/includes.chroot/opt/jukebox/storage_manage.py'
spec = importlib.util.spec_from_file_location('machine_storage', SCRIPT)
storage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(storage)


class Response(io.BytesIO):
    status = 200
    headers = {}


class MachineStorageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / 'musicas').mkdir()
        self.data = b'music content'
        self.item = dict(path='01.mp3', size=len(self.data), sha256=hashlib.sha256(self.data).hexdigest(), url='https://example.test/01')
        self.album = dict(schema=1, id='album-1', version='v1', genre='Rock', artist='Ana', title='CD', files=[self.item])
        self.index = dict(schema=1, version='catalog-v1', albums=[dict(id='album-1', version='v1', manifest_url='https://example.test/album')])

    def sync(self):
        with patch.object(storage.catalog, 'fetch_json', side_effect=lambda url: self.index if url.endswith('/index') else self.album), \
             patch.object(storage.catalog, 'request', return_value=Response(self.data)):
            return storage.catalog.sync(self.root, 'https://example.test/index', 0)

    def rows(self):
        return storage.operate(self.root, {'action': 'list'})['rows']

    def remove(self, album, paths=None):
        return storage.operate(self.root, {'action': 'remove', 'album': album,
            'protected_paths': paths or [], 'working_dir': str(self.root)})

    def test_remove_suppresses_download_even_after_server_update_and_restore(self):
        self.sync()
        row = self.rows()[0]
        removed = self.remove(row)
        self.assertTrue(removed['changed'])
        self.assertTrue(removed['rows'][0]['removed'])
        self.assertFalse((self.root / row['path']).exists())
        self.album.update(version='v2', genre='Gospel')
        self.index['albums'][0]['version'] = 'v2'
        with patch.object(storage.catalog, 'fetch_json', return_value=self.index) as fetched, \
             patch.object(storage.catalog, 'download') as download:
            self.assertFalse(storage.catalog.sync(self.root, 'https://example.test/index', 0))
        self.assertEqual(fetched.call_count, 1)
        download.assert_not_called()
        result = storage.operate(self.root, {'action': 'restore', 'album': removed['rows'][0]})
        self.assertTrue(result['restore'])
        self.assertTrue(self.sync())
        self.assertEqual((self.root / 'musicas/Gospel/Ana/CD/01.mp3').read_bytes(), self.data)

    def test_current_or_queued_track_blocks_removal_without_saving_exclusion(self):
        self.sync()
        row = self.rows()[0]
        for protected in (str(self.root / row['path'] / '01.mp3'), row['path'] + '/01.mp3'):
            with self.subTest(path=protected), self.assertRaisesRegex(ValueError, 'tocando ou com músicas na fila'):
                self.remove(row, [protected])
        self.assertTrue((self.root / row['path'] / '01.mp3').exists())
        self.assertFalse((self.root / 'catalog-exclusions.json').exists())

    def test_parent_album_cannot_remove_another_album_inside_it(self):
        self.sync()
        nested = self.root / 'musicas/Rock/Ana/CD/Disco 2'
        nested.mkdir()
        (nested / '02.mp3').write_bytes(self.data)
        parent = next(row for row in self.rows() if row['id'] == 'album-1')
        with self.assertRaisesRegex(ValueError, 'contém outros álbuns'):
            self.remove(parent)
        self.assertTrue((nested / '02.mp3').exists())
        self.assertFalse((self.root / 'catalog-exclusions.json').exists())

    def test_cleanup_removes_only_backups_and_partials_owned_by_album(self):
        self.sync()
        backups = self.root / '.downloads'
        for name, aid, marker in [('old', 'album-1', '.jukebox-album.json'),
                                  ('partial', 'album-1', '.jukebox-download.json'),
                                  ('other', 'album-2', '.jukebox-album.json')]:
            folder = backups / name
            folder.mkdir()
            (folder / marker).write_text(json.dumps({'id': aid}))
            (folder / 'song.mp3.part').write_bytes(b'bytes')
        unknown = backups / 'unknown'
        unknown.mkdir()
        self.remove(self.rows()[0])
        self.assertFalse((backups / 'old').exists())
        self.assertFalse((backups / 'partial').exists())
        self.assertTrue((backups / 'other').exists())
        self.assertTrue(unknown.exists())

    def test_usb_album_is_removable_and_backgrounds_are_not_listed(self):
        folder = self.root / 'musicas/Piseiro/Beto/CD'
        folder.mkdir(parents=True)
        (folder / 'song.mp3').write_bytes(self.data)
        background = self.root / 'musicas/fundos'
        background.mkdir()
        (background / 'loop.mp4').write_bytes(b'video')
        row = self.rows()[0]
        self.assertFalse(row['online'])
        self.assertEqual(len(self.rows()), 1)
        result = self.remove(row)
        self.assertEqual(result['rows'], [])
        self.assertTrue((background / 'loop.mp4').exists())

    def test_stale_selection_does_not_remove_renamed_album(self):
        self.sync()
        row = self.rows()[0]
        self.album.update(version='v2', title='Novo CD')
        self.index['albums'][0]['version'] = 'v2'
        self.sync()
        with self.assertRaisesRegex(ValueError, 'álbum mudou'):
            self.remove(row)
        self.assertTrue((self.root / 'musicas/Rock/Ana/Novo CD/01.mp3').exists())

    def test_interruption_after_exclusion_is_saved_cannot_redownload_album(self):
        self.sync()
        row = self.rows()[0]
        with patch.object(storage.os, 'rename', side_effect=OSError('shutdown')):
            with self.assertRaises(OSError):
                self.remove(row)
        with patch.object(storage.catalog, 'fetch_json', return_value=self.index) as fetched:
            self.assertFalse(storage.catalog.sync(self.root, 'https://example.test/index', 0))
        self.assertEqual(fetched.call_count, 1)
        # Retry is possible while the intact album is still listed locally.
        self.assertTrue(self.remove(self.rows()[0])['changed'])

    def test_unknown_queue_and_symlink_downloads_block_removal(self):
        self.sync()
        row = self.rows()[0]
        with self.assertRaisesRegex(ValueError, 'conferir a fila'):
            storage.operate(self.root, {'action': 'remove', 'album': row})
        outside = self.root / 'external'
        outside.mkdir()
        import shutil
        shutil.rmtree(self.root / '.downloads')
        (self.root / '.downloads').symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'não pode ser um link'):
            self.remove(row)
        self.assertTrue((self.root / row['path']).exists())

    def test_corrupt_exclusion_list_stops_sync_instead_of_reenabling_downloads(self):
        (self.root / 'catalog-exclusions.json').write_text('{')
        with patch.object(storage.catalog, 'fetch_json', return_value=self.index), self.assertRaises(ValueError):
            storage.catalog.sync(self.root, 'https://example.test/index', 0)
