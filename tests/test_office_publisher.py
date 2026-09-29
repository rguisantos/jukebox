import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'distro/tools/jukebox-publisher-gui.py'
spec = importlib.util.spec_from_file_location('office_publisher_test', SCRIPT)
office = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = office
spec.loader.exec_module(office)


def tagged_mp3(genre):
    value = b'\x03' + genre.encode()
    frame = b'TCON' + len(value).to_bytes(4, 'big') + b'\0\0' + value
    size = len(frame)
    return b'ID3\x03\0\0' + bytes([0, 0, size >> 7, size & 127]) + frame + b'audio'


class OfficeScanTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_select_collection_or_single_album_and_edit_missing_genre(self):
        tagged = self.root / 'Ana' / 'Disco A'
        tagged.mkdir(parents=True)
        (tagged / '01.mp3').write_bytes(tagged_mp3('Rock'))
        (tagged / 'capa.jpg').write_bytes(b'cover')
        untagged = self.root / 'Beto' / 'Disco B'
        untagged.mkdir(parents=True)
        (untagged / '01.mp3').write_bytes(b'audio')
        albums = office.scan_folder(self.root)
        self.assertEqual([(a.artist, a.title, a.genre) for a in albums],
                         [('Ana', 'Disco A', 'Rock'), ('Beto', 'Disco B', '')])
        self.assertFalse(albums[1].blocked)
        self.assertEqual(office.scan_folder(tagged)[0].genre, 'Rock')

    def test_nested_cd_is_blocked_before_upload(self):
        album = self.root / 'Ana' / 'Disco'
        album.mkdir(parents=True)
        (album / '01.mp3').write_bytes(tagged_mp3('Pop'))
        (album / 'CD2').mkdir()
        draft = office.scan_folder(self.root)[0]
        self.assertTrue(draft.blocked)
        self.assertIn('CD2', draft.note)

    def test_genres_are_canonical_or_wait_for_review(self):
        album = self.root / 'Ana' / 'Disco'
        album.mkdir(parents=True)
        track = album / '01.mp3'
        policy = office.engine.GenrePolicy()
        track.write_bytes(tagged_mp3('Música Religiosa;Gospel'))
        draft = office.scan_folder(album, policy)[0]
        self.assertEqual(draft.genre, 'Gospel')
        track.write_bytes(tagged_mp3('Gospel;Funk'))
        draft = office.scan_folder(album, policy)[0]
        self.assertEqual(draft.genre, '')
        self.assertFalse(draft.blocked)

    def test_genre_policy_persists_and_normalizes_old_drafts(self):
        original = office.GENRES_FILE
        office.GENRES_FILE = self.root / 'genres.json'
        self.addCleanup(setattr, office, 'GENRES_FILE', original)
        policy = office.engine.GenrePolicy()
        policy.add_genre('Axé')
        policy.add_alias('Axé Music', 'Axé')
        office.save_policy(policy)
        loaded = office.load_policy()
        self.assertEqual(loaded.resolve('axé music'), 'Axé')
        self.assertEqual(os.stat(office.GENRES_FILE).st_mode & 0o777, 0o600)
        draft = office.AlbumDraft(self.root, 'Ana', 'Disco', 'Música Religiosa;Gospel', 1, 5)
        office.normalize_drafts({'a': draft}, loaded)
        self.assertEqual(draft.genre, 'Gospel')

    def test_settings_store_only_connection_details(self):
        original = office.SETTINGS_FILE
        office.SETTINGS_FILE = self.root / 'settings.json'
        self.addCleanup(setattr, office, 'SETTINGS_FILE', original)
        office.save_settings(office.DEFAULTS.copy())
        self.assertEqual(office.load_settings(), office.DEFAULTS)
        self.assertEqual(os.stat(office.SETTINGS_FILE).st_mode & 0o777, 0o600)

    def test_manual_genre_and_selection_survive_restart(self):
        original = office.DRAFTS_FILE
        office.DRAFTS_FILE = self.root / 'drafts.json'
        self.addCleanup(setattr, office, 'DRAFTS_FILE', original)
        album = self.root / 'Ana' / 'Disco'
        album.mkdir(parents=True)
        draft = office.AlbumDraft(album, 'Ana', 'Disco', 'Piseiro', 2, 100)
        office.save_drafts({str(album): draft})
        loaded = office.load_drafts()
        self.assertEqual(loaded[str(album)].genre, 'Piseiro')
        self.assertEqual(os.stat(office.DRAFTS_FILE).st_mode & 0o777, 0o600)


if __name__ == '__main__':
    unittest.main()
