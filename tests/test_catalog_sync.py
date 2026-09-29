import importlib.util
import io
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).resolve().parents[1] / 'distro/config/includes.chroot/opt/jukebox/catalog_sync.py'
spec = importlib.util.spec_from_file_location('sync', MODULE)
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)

class Response(io.BytesIO):
    def __init__(self, data, status=200, headers=None):
        super().__init__(data)
        self.status = status
        self.headers = headers or {}

class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.data = b'music content'
        self.item = dict(path='01.mp3', size=len(self.data), sha256=hashlib.sha256(self.data).hexdigest(), url='https://example.test/01')
        self.album = dict(schema=1, id='album-1', version='v1', genre='Rock', artist='Artista', title='Disco', files=[self.item])
        self.index = dict(schema=1, version='catalog-v1', albums=[dict(id='album-1', version='v1', manifest_url='https://example.test/album')])

    def run_sync(self):
        with patch.object(sync, 'fetch_json', side_effect=lambda url: self.index if url.endswith('/index') else self.album), patch.object(sync, 'request', side_effect=lambda *a, **k: Response(self.data)):
            return sync.sync(self.root, 'https://example.test/index', 1000000)

    def test_incremental_preserves_local_and_operational_data(self):
        (self.root / 'jukebox.db').write_bytes(b'operational database untouched')
        self.assertTrue(self.run_sync())
        self.assertFalse(self.run_sync())
        self.assertEqual((self.root / 'musicas/Rock/Artista/Disco/01.mp3').read_bytes(), self.data)
        self.assertEqual((self.root / 'jukebox.db').read_bytes(), b'operational database untouched')

    def test_missing_or_corrupted_track_is_restored_without_version_change(self):
        self.run_sync()
        albumdir = self.root / 'musicas/Rock/Artista/Disco'
        (albumdir / '01.mp3').unlink()
        self.assertTrue(self.run_sync())
        self.assertEqual((albumdir / '01.mp3').read_bytes(), self.data)
        (albumdir / '01.mp3').write_bytes(b'x' * len(self.data))
        os.utime(albumdir / '01.mp3', ns=(1, 1))
        self.assertTrue(self.run_sync())
        self.assertEqual((albumdir / '01.mp3').read_bytes(), self.data)
        self.assertFalse(self.run_sync())

    def test_old_marker_is_upgraded_without_republishing_album(self):
        self.run_sync()
        albumdir = self.root / 'musicas/Rock/Artista/Disco'
        marker = albumdir / '.jukebox-album.json'
        marker.write_text(json.dumps({'id': 'album-1', 'version': 'v1'}))
        self.assertFalse(self.run_sync())
        self.assertIn('files', json.loads(marker.read_text()))
        self.assertFalse(self.run_sync())

    def test_bad_hash_never_publishes_album(self):
        self.item['sha256'] = '0' * 64
        with self.assertRaises(ValueError): self.run_sync()
        self.assertFalse((self.root / 'musicas/Rock/Artista/Disco').exists())

    def edit_metadata(self):
        self.album.update(genre='Gospel', artist='Novo Artista', title='Novo Disco', version='v2')
        self.index['albums'][0]['version'] = 'v2'

    def test_metadata_edit_moves_album_without_download_or_duplicate(self):
        self.run_sync()
        old = self.root / 'musicas/Rock/Artista/Disco'
        inode = (old / '01.mp3').stat().st_ino
        self.edit_metadata()
        with patch.object(sync, 'download', side_effect=AssertionError('No media download')):
            self.assertTrue(self.run_sync())
        new = self.root / 'musicas/Gospel/Novo Artista/Novo Disco'
        self.assertFalse(old.exists())
        self.assertEqual((new / '01.mp3').stat().st_ino, inode)
        self.assertFalse(self.run_sync())

    def test_metadata_edit_recovers_after_rename_before_state_save(self):
        self.run_sync()
        self.edit_metadata()
        original = sync.atomic_json
        def fail_state(path, data):
            if path.name == 'catalog-state.json':
                raise OSError('simulated shutdown')
            original(path, data)
        with patch.object(sync, 'atomic_json', side_effect=fail_state):
            with self.assertRaises(OSError):
                self.run_sync()
        self.assertTrue(self.run_sync())
        self.assertFalse(self.run_sync())
        self.assertFalse((self.root / 'musicas/Rock/Artista/Disco').exists())

    def test_metadata_edit_restores_missing_track(self):
        self.run_sync()
        (self.root / 'musicas/Rock/Artista/Disco/01.mp3').unlink()
        self.edit_metadata()
        self.assertTrue(self.run_sync())
        self.assertEqual((self.root / 'musicas/Gospel/Novo Artista/Novo Disco/01.mp3').read_bytes(), self.data)
        self.assertFalse((self.root / 'musicas/Rock/Artista/Disco').exists())

    def test_metadata_edit_preserves_foreign_destination(self):
        self.run_sync()
        self.edit_metadata()
        foreign = self.root / 'musicas/Gospel/Novo Artista/Novo Disco'
        foreign.mkdir(parents=True)
        (foreign / 'local.mp3').write_bytes(b'local')
        with self.assertRaises(ValueError):
            self.run_sync()
        self.assertTrue((self.root / 'musicas/Rock/Artista/Disco/01.mp3').exists())
        self.assertEqual((foreign / 'local.mp3').read_bytes(), b'local')

    def test_resume_valid_range(self):
        target = self.root / '01.mp3'
        target.with_name('01.mp3.part').write_bytes(self.data[:4])
        with patch.object(sync, 'request', return_value=Response(self.data[4:], 206, {'Content-Range': f'bytes 4-{len(self.data)-1}/{len(self.data)}'})) as req:
            sync.download(self.item, target, 1000000)
        self.assertEqual(req.call_args.args[1]['Range'], 'bytes=4-')
        self.assertEqual(target.read_bytes(), self.data)

    def test_range_ignored_restarts_safely(self):
        target = self.root / '01.mp3'
        target.with_name('01.mp3.part').write_bytes(b'old')
        with patch.object(sync, 'request', return_value=Response(self.data)):
            sync.download(self.item, target, 1000000)
        self.assertEqual(target.read_bytes(), self.data)

    def test_zero_download_limit_does_not_sleep(self):
        target = self.root / 'unlimited.mp3'
        with patch.object(sync, 'request', return_value=Response(self.data)), \
                patch.object(sync.time, 'sleep') as sleep:
            sync.download(self.item, target, 0)
        self.assertEqual(target.read_bytes(), self.data)
        sleep.assert_not_called()

    def test_bad_range_rejected(self):
        target = self.root / '01.mp3'
        target.with_name('01.mp3.part').write_bytes(self.data[:4])
        with patch.object(sync, 'request', return_value=Response(self.data[4:], 206, {'Content-Range': 'bytes 0-8/9'})):
            with self.assertRaises(ValueError): sync.download(self.item, target, 1000000)
        self.assertFalse(target.exists())

    def test_update_uses_complete_directory_and_keeps_omitted_tracks(self):
        self.run_sync()
        albumdir = self.root / 'musicas/Rock/Artista/Disco'
        (albumdir / 'local.mp3').write_bytes(b'keep')
        self.album['version'] = self.index['albums'][0]['version'] = 'v2'
        self.data = b'new music'
        self.item.update(size=len(self.data), sha256=hashlib.sha256(self.data).hexdigest())
        self.assertTrue(self.run_sync())
        self.assertEqual((albumdir / '01.mp3').read_bytes(), self.data)
        self.assertEqual((albumdir / 'local.mp3').read_bytes(), b'keep')

    def test_foreign_album_is_not_overwritten(self):
        albumdir = self.root / 'musicas/Rock/Artista/Disco'
        albumdir.mkdir(parents=True)
        with self.assertRaises(ValueError): self.run_sync()

    def test_paths_rejected(self):
        for name in ('../escape', '/etc/passwd', '..', '.hidden', 'a\\b', 'a\x00b'):
            with self.subTest(name=name), self.assertRaises(ValueError): sync.component(name)

    def test_request_identifies_catalog_client(self):
        with patch.object(sync.OPENER, 'open', return_value=Response(b'{}')) as opened:
            sync.request('https://example.test/index.json')
        self.assertEqual(opened.call_args.args[0].get_header('User-agent'), 'jukebox-catalog/1.0')

if __name__ == '__main__': unittest.main()
