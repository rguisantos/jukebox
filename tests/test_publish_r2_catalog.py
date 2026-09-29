import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'distro/tools/publish-r2-catalog.py'
spec = importlib.util.spec_from_file_location('publish_r2_catalog', SCRIPT)
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


def mp3_with_genre(value):
    payload = b'\x03' + value.encode('utf-8')
    frame = b'TCON' + len(payload).to_bytes(4, 'big') + b'\0\0' + payload
    size = len(frame)
    header = b'ID3\x03\0\0' + bytes([(size >> 21) & 127, (size >> 14) & 127,
                                         (size >> 7) & 127, size & 127])
    return header + frame + b'audio'


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / 'incoming'
        self.source.mkdir()
        self.base = 'https://pub.example.r2.dev'
        self.empty_index = {'schema': 1, 'version': 'old', 'albums': []}

    def album(self, artist, title, genre):
        directory = self.source / artist / title
        directory.mkdir(parents=True)
        (directory / '01 - Faixa.mp3').write_bytes(mp3_with_genre(genre))
        return directory

    def test_genre_alias_and_multiple_equivalent_tags(self):
        policy = publisher.GenrePolicy()
        self.assertEqual(policy.resolve('musica religiosa;GOSPEL'), 'Gospel')
        album = self.album('Ana', 'Disco A', 'Música Religiosa;Gospel')
        self.assertEqual(publisher.choose_genre(album, 'Ana', publisher.load_overrides(None), policy=policy), 'Gospel')
        (album / '02.mp3').write_bytes(mp3_with_genre('gospel'))
        self.assertEqual(publisher.choose_genre(album, 'Ana', publisher.load_overrides(None), policy=policy), 'Gospel')

    def test_unknown_and_distinct_genres_require_review(self):
        policy = publisher.GenrePolicy()
        for raw in ('Gospel;Funk', 'Novo Estilo', ';'):
            with self.subTest(raw=raw), self.assertRaises(publisher.GenreReviewNeeded):
                policy.resolve(raw)
        with self.assertRaises(ValueError):
            policy.add_genre('Gospel;Funk')
        with self.assertRaises(ValueError):
            policy.add_genre('gospel')
        with self.assertRaises(ValueError):
            policy.add_alias('MUSICA RELIGIOSA', 'Gospel')

    def test_batch_preserves_existing_index_and_detects_genres_from_tags(self):
        self.album('Ana', 'Disco A', 'Rock')
        self.album('Beto', 'Disco B', 'Sertanejo')
        old = {'id': 'album-old', 'version': '1', 'manifest_url': self.base + '/old.json',
               'artist': 'Outro', 'title': 'Outro Disco', 'genre': 'Piseiro'}
        index = {'schema': 1, 'version': 'old', 'albums': [old]}
        output = self.root / 'release'
        changes = publisher.prepare(self.source, self.base, output, publisher.load_overrides(None), index=index)
        self.assertEqual(len(changes), 2)
        result = json.loads((output / 'index.json').read_text())
        self.assertEqual(len(result['albums']), 3)
        self.assertEqual(next(e for e in result['albums'] if e['id'] == 'album-old'), old)
        self.assertEqual({c[2] for c in changes}, {'Rock', 'Sertanejo'})

    def test_no_change_does_not_replace_index(self):
        album = self.album('Ana', 'Disco A', 'Rock')
        with tempfile.TemporaryDirectory() as previous:
            _, _, entry = publisher.build_album(album, 'Rock', 'Ana', self.base, previous)
        index = {'schema': 1, 'version': 'before', 'albums': [entry]}
        output = self.root / 'release'
        self.assertEqual(publisher.prepare(self.source, self.base, output,
                                           publisher.load_overrides(None), index=index), [])
        self.assertFalse((output / 'index.json').exists())

    def test_existing_pilot_album_is_recognized_without_index_metadata(self):
        self.album('Ana', 'Disco A', 'Rock')
        full = publisher.build_album(self.source / 'Ana' / 'Disco A', 'Rock', 'Ana',
                                     self.base, self.root / 'previous')
        aid, version, entry = full
        legacy = {key: entry[key] for key in ('id', 'version', 'manifest_url')}
        manifest = (self.root / 'previous' / 'albums' / aid / version / 'manifest.json').read_bytes()
        with patch.object(publisher.urllib.request, 'urlopen', return_value=io.BytesIO(manifest)):
            changes = publisher.prepare(self.source, self.base, self.root / 'release',
                                        publisher.load_overrides(None),
                                        index={'schema': 1, 'version': version, 'albums': [legacy]})
        self.assertEqual(changes, [])

    def test_ambiguous_tags_require_override(self):
        album = self.album('Ana', 'Disco A', 'Rock')
        (album / '02.mp3').write_bytes(mp3_with_genre('Pop'))
        with self.assertRaisesRegex(ValueError, 'gêneros diferentes'):
            publisher.discover(self.source, publisher.load_overrides(None))
        overrides = {'albums': {'Ana/Disco A': 'Pop Rock'}, 'artists': {}}
        self.assertEqual(publisher.discover(self.source, overrides)[0][2], 'Pop Rock')

    def test_genre_artist_album_layout_uses_genre_folder(self):
        album = self.source / 'Piseiro' / 'Ana' / 'Disco A'
        album.mkdir(parents=True)
        (album / '01.mp3').write_bytes(mp3_with_genre('Outro Estilo'))
        jobs = publisher.discover(self.source, publisher.load_overrides(None),
                                  layout='genre-artist-album')
        self.assertEqual(jobs, [(album, 'Ana', 'Piseiro')])

    def test_genre_change_rejected_for_existing_album(self):
        self.album('Ana', 'Disco A', 'Rock')
        old = {'id': 'album-previous', 'version': '1', 'manifest_url': self.base + '/old.json',
               'artist': 'Ana', 'title': 'Disco A', 'genre': 'Pop'}
        with self.assertRaisesRegex(ValueError, 'já publicado'):
            publisher.prepare(self.source, self.base, self.root / 'out', publisher.load_overrides(None),
                              index={'schema': 1, 'version': 'x', 'albums': [old]})

    def test_publish_sends_index_last(self):
        output = self.root / 'out'
        output.mkdir()
        (output / 'index.json').write_text(json.dumps({'albums': [
            {'manifest_url': self.base + '/albums/new/manifest.json'}]}))
        (output / '.remote-version').write_text('old')
        with patch.object(publisher.subprocess, 'run') as run, \
                patch.object(publisher, 'existing_index', return_value=self.empty_index):
            publisher.publish(output, 'jukebox', 'jukebox-r2', 'https://example.test', self.base)
        self.assertEqual(run.call_count, 2)
        self.assertIn('/albums/', run.call_args_list[0].args[0][-2])
        self.assertEqual(run.call_args_list[1].args[0][8], 's3://jukebox/index.json')

    def test_remote_metadata_edit_keeps_id_and_media_urls(self):
        album = self.album('Ana', 'Disco A', 'Rock')
        aid, version, entry = publisher.build_album(album, 'Rock', 'Ana', self.base, self.root / 'before')
        original = json.loads((self.root / 'before/albums' / aid / version / 'manifest.json').read_text())
        old_index = {'schema': 1, 'version': 'old', 'albums': [entry]}
        output = self.root / 'edits'
        edits = {aid: {'artist': 'Ana Maria', 'title': 'Disco B', 'genre': 'Gospel'}}
        with patch.object(publisher.urllib.request, 'urlopen', return_value=io.BytesIO(json.dumps(original).encode())):
            changes = publisher.prepare_metadata_edits(edits, self.base, output, publisher.GenrePolicy(), old_index)
        updated = json.loads(next((output / 'albums').rglob('manifest.json')).read_text())
        self.assertEqual(updated['id'], aid)
        self.assertNotEqual(updated['version'], version)
        self.assertEqual(updated['files'], original['files'])
        self.assertEqual(updated['genre'], 'Gospel')
        self.assertEqual(len(list((output / 'albums').rglob('*.*'))), 1)
        # A future media update keeps the edited album's existing ID.
        index = json.loads((output / 'index.json').read_text())
        updated_folder = self.source / 'Ana Maria' / 'Disco B'
        updated_folder.mkdir(parents=True)
        (updated_folder / '01.mp3').write_bytes(mp3_with_genre('Gospel'))
        jobs = [(updated_folder, 'Ana Maria', 'Gospel')]
        result = publisher.prepare_jobs(jobs, self.base, self.root / 'next', index)
        self.assertEqual(result[0][3], aid)

    def test_remote_edit_rejects_duplicate_artist_album(self):
        album = self.album('Ana', 'Disco A', 'Rock')
        aid, version, entry = publisher.build_album(album, 'Rock', 'Ana', self.base, self.root / 'before')
        manifest = (self.root / 'before/albums' / aid / version / 'manifest.json').read_bytes()
        other = dict(entry, id='album-other', artist='Beto', title='Disco B')
        index = {'schema': 1, 'version': 'old', 'albums': [entry, other]}
        edits = {aid: {'artist': 'Beto', 'title': 'Disco B', 'genre': 'Gospel'}}
        with patch.object(publisher.urllib.request, 'urlopen', return_value=io.BytesIO(manifest)):
            with self.assertRaisesRegex(ValueError, 'duplicado'):
                publisher.prepare_metadata_edits(edits, self.base, self.root / 'edits', publisher.GenrePolicy(), index)

    def test_remote_edit_rejects_catalog_changed_since_opening(self):
        with self.assertRaisesRegex(ValueError, 'catálogo mudou'):
            publisher.prepare_metadata_edits({}, self.base, self.root / 'edits', publisher.GenrePolicy(),
                                            self.empty_index, expected_version='stale')

    def test_publish_refuses_stale_preview(self):
        output = self.root / 'out'
        output.mkdir()
        (output / 'index.json').write_text(json.dumps({'albums': [
            {'manifest_url': self.base + '/albums/new/manifest.json'}]}))
        (output / '.remote-version').write_text('old')
        with patch.object(publisher.subprocess, 'run') as run, \
                patch.object(publisher, 'existing_index', return_value={'schema': 1, 'version': 'new', 'albums': []}):
            with self.assertRaisesRegex(ValueError, 'mudou'):
                publisher.publish(output, 'jukebox', 'jukebox-r2', 'https://example.test', self.base)
        run.assert_not_called()

    def test_publish_reports_upload_lines(self):
        output = self.root / 'out'
        output.mkdir()
        (output / 'index.json').write_text(json.dumps({'albums': [
            {'manifest_url': self.base + '/albums/new/manifest.json'}]}))
        (output / '.remote-version').write_text('old')
        class Process:
            def __init__(self):
                self.stdout = iter(['upload: faixa.mp3 to s3://jukebox/albums/faixa.mp3\n'])
            def wait(self): return 0
        messages = []
        with patch.object(publisher.subprocess, 'Popen', side_effect=lambda *a, **k: Process()) as popen, \
                patch.object(publisher, 'existing_index', return_value=self.empty_index):
            publisher.publish(output, 'jukebox', 'jukebox-r2', 'https://example.test',
                              self.base, progress=messages.append)
        self.assertEqual(len(messages), 2)
        self.assertEqual(popen.call_count, 2)
        self.assertIn('--no-progress', popen.call_args.args[0])


if __name__ == '__main__':
    unittest.main()
