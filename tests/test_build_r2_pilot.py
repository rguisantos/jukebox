import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import io
from urllib.parse import unquote, urlsplit

SCRIPT = Path(__file__).resolve().parents[1] / 'distro/tools/build-r2-pilot.py'
spec = importlib.util.spec_from_file_location('pilot', SCRIPT)
pilot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pilot)
SYNC = Path(__file__).resolve().parents[1] / 'distro/config/includes.chroot/opt/jukebox/catalog_sync.py'
sync_spec = importlib.util.spec_from_file_location('catalog_sync_pilot', SYNC)
sync = importlib.util.module_from_spec(sync_spec)
sync_spec.loader.exec_module(sync)


class PilotTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.album = self.root / 'Artista' / 'Disco'
        self.album.mkdir(parents=True)
        (self.album / '01 - Canção.mp3').write_bytes(b'example mp3')
        (self.album / 'cover.jpg').write_bytes(b'example cover')
        self.out = self.root / 'out'

    def build(self):
        return pilot.build(self.album, 'Rock', 'Artista', 'https://musicas.example.com', self.out)

    def test_manifest_matches_client_contract_and_is_repeatable(self):
        aid, version = self.build()
        index = json.loads((self.out / 'index.json').read_text())
        manifest = json.loads((self.out / 'albums' / aid / version / 'manifest.json').read_text())
        self.assertEqual(index['albums'][0]['version'], manifest['version'])
        self.assertEqual(manifest['genre'], 'Rock')
        self.assertIn('Can%C3%A7%C3%A3o.mp3', manifest['files'][0]['url'])
        self.assertEqual(self.build(), (aid, version))
        (self.album / '01 - Canção.mp3').write_bytes(b'new example')
        self.assertNotEqual(self.build()[1], version)
        self.assertTrue((self.out / 'albums' / aid / version / 'manifest.json').exists())

    def test_rejects_symlink_and_subdirectory(self):
        (self.album / 'link.mp3').symlink_to(self.album / '01 - Canção.mp3')
        with self.assertRaises(ValueError):
            self.build()
        (self.album / 'link.mp3').unlink()
        (self.album / 'CD2').mkdir()
        with self.assertRaises(ValueError):
            self.build()

    def test_requires_https_and_rejects_invalid_names(self):
        with self.assertRaises(ValueError):
            pilot.build(self.album, 'Rock', 'Artista', 'http://test', self.out)
        with self.assertRaises(ValueError):
            pilot.build(self.album, '../Rock', 'Artista', 'https://test', self.out)

    def test_generated_catalog_is_consumed_by_existing_client(self):
        self.build()
        class Response(io.BytesIO):
            status = 200
            headers = {}

        def fetch_json(url):
            return json.loads((self.out / unquote(urlsplit(url).path.lstrip('/'))).read_text())

        def request(url, headers=None):
            return Response((self.out / unquote(urlsplit(url).path.lstrip('/'))).read_bytes())

        data_dir = self.root / 'machine-data'
        with patch.object(sync, 'fetch_json', side_effect=fetch_json), patch.object(sync, 'request', side_effect=request):
            self.assertTrue(sync.sync(data_dir, 'https://musicas.example.com/index.json', 1000000))
            self.assertFalse(sync.sync(data_dir, 'https://musicas.example.com/index.json', 1000000))
        self.assertEqual((data_dir / 'musicas/Rock/Artista/Disco/01 - Canção.mp3').read_bytes(), b'example mp3')


if __name__ == '__main__':
    unittest.main()
