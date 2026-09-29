#!/usr/bin/env python3
"""Publish a batch of Artist/Album folders to the jukebox R2 catalog.

No third-party Python dependencies. Existing remote index entries are retained;
uploads are immutable and the new index is written last.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import unicodedata
import urllib.request
from urllib.parse import quote

MEDIA = {'.mp3', '.mp4', '.wav', '.wmv', '.mpeg'}
COVERS = {'.jpg', '.jpeg', '.png', '.webp'}
DEFAULT_GENRES = ('Piseiro', 'Sertanejo', 'Forró', 'Gospel', 'Rock', 'Pop',
                  'Funk', 'Pagode', 'Samba', 'MPB', 'Eletrônica', 'Infantil', 'Outros')
DEFAULT_ALIASES = {'Música Religiosa': 'Gospel'}


class GenreReviewNeeded(ValueError):
    """A tag needs the operator to choose a canonical genre."""


def genre_key(value):
    return ' '.join(''.join(c for c in unicodedata.normalize('NFKD', value.casefold())
                            if not unicodedata.combining(c)).split())


def valid_genre(value):
    name = valid_component(value.strip())
    if ';' in name:
        raise ValueError('Um gênero não pode conter ponto e vírgula')
    return name


class GenrePolicy:
    def __init__(self, genres=None, aliases=None):
        self.genres = []
        self.aliases = {}
        for name in (genres if genres is not None else DEFAULT_GENRES):
            self.add_genre(name)
        for alias, target in (aliases if aliases is not None else DEFAULT_ALIASES).items():
            self.add_alias(alias, target)

    def add_genre(self, name):
        name = valid_genre(name)
        if genre_key(name) in self.aliases:
            raise ValueError(f'Esse nome já é um sinônimo: {name}')
        if any(genre_key(old) == genre_key(name) for old in self.genres):
            raise ValueError(f'Gênero já cadastrado: {name}')
        self.genres.append(name)
        return name

    def add_alias(self, alias, target):
        alias, target = valid_genre(alias), self.canonical(target)
        key = genre_key(alias)
        if any(genre_key(name) == key for name in self.genres) or key in self.aliases:
            raise ValueError(f'Nome já cadastrado: {alias}')
        self.aliases[key] = target

    def canonical(self, name):
        key = genre_key(valid_genre(name))
        for candidate in self.genres:
            if genre_key(candidate) == key:
                return candidate
        if key in self.aliases:
            return self.aliases[key]
        raise GenreReviewNeeded(f'Gênero não cadastrado: {name}')

    def resolve(self, raw):
        parts = [part.strip() for part in raw.split(';') if part.strip()]
        if not parts:
            raise GenreReviewNeeded('Tag sem gênero; selecione um gênero cadastrado')
        resolved = {self.canonical(part) for part in parts}
        if len(resolved) != 1:
            raise GenreReviewNeeded(f'Tag combina gêneros diferentes: {raw}')
        return resolved.pop()

    def as_json(self):
        return {'genres': self.genres, 'aliases': self.aliases}


def valid_component(value):
    if (not isinstance(value, str) or not value or value in {'.', '..'} or value.startswith('.')
            or len(value.encode('utf-8')) > 180
            or any(c in value for c in '/\\\x00')
            or any(ord(c) < 32 for c in value)):
        raise ValueError(f'Nome inválido: {value!r}')
    return value


def build_album(album_dir, genre, artist, base_url, output, album_id=None):
    album_dir, output = Path(album_dir), Path(output)
    title, artist = (valid_component(v) for v in (album_dir.name, artist))
    genre = valid_genre(genre)
    if output.resolve() == album_dir.resolve() or album_dir.resolve() in output.resolve().parents:
        raise ValueError('Saída precisa ficar fora da pasta do álbum')
    files = sorted(album_dir.iterdir())
    if not files or any(not p.is_file() or p.is_symlink() for p in files):
        raise ValueError(f'Álbum deve conter somente arquivos comuns: {album_dir}')
    if any(p.suffix.lower() not in MEDIA | COVERS for p in files):
        raise ValueError(f'Formato não aceito: {album_dir}')
    if not any(p.suffix.lower() in MEDIA for p in files):
        raise ValueError(f'Álbum sem faixas: {album_dir}')
    names = [valid_component(p.name) for p in files]
    if len({name.casefold() for name in names}) != len(names):
        raise ValueError(f'Nomes de arquivos conflitantes: {album_dir}')
    aid = valid_component(album_id) if album_id else 'album-' + hashlib.sha256('\0'.join((genre, artist, title)).encode()).hexdigest()[:20]
    checked = []
    release_hash = hashlib.sha256()
    release_hash.update(json.dumps([genre, artist, title], ensure_ascii=False).encode())
    for p in files:
        digest = hashlib.sha256()
        with p.open('rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
        size = p.stat().st_size
        if not size:
            raise ValueError(f'Arquivo vazio: {p}')
        checksum = digest.hexdigest()
        release_hash.update(json.dumps([p.name, size, checksum], ensure_ascii=False).encode())
        checked.append((p, size, checksum))
    version = release_hash.hexdigest()[:20]
    release = output / 'albums' / aid / version
    release.mkdir(parents=True, exist_ok=True)
    items = []
    for p, size, checksum in checked:
        target = release / p.name
        if target.exists():
            if target.stat().st_size != size or hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
                raise ValueError(f'Arquivo preparado diverge: {target}')
        else:
            shutil.copyfile(p, target)
            if target.stat().st_size != size:
                raise ValueError(f'Arquivo mudou durante o preparo: {p}')
            copied = hashlib.sha256()
            with target.open('rb') as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b''):
                    copied.update(chunk)
            if copied.hexdigest() != checksum:
                raise ValueError(f'Arquivo mudou durante o preparo: {p}')
        items.append({'path': p.name, 'url': f'{base_url.rstrip("/")}/albums/{aid}/{version}/{quote(p.name)}',
                      'size': size, 'sha256': checksum})
    manifest = {'schema': 1, 'id': aid, 'version': version, 'genre': genre,
                'artist': artist, 'title': title, 'files': items}
    target = release / 'manifest.json'
    serialized = json.dumps(manifest, ensure_ascii=False, indent=2) + '\n'
    if target.exists() and target.read_text() != serialized:
        raise ValueError('Manifesto imutável diverge')
    target.write_text(serialized, encoding='utf-8')
    return aid, version, {'id': aid, 'version': version,
                          'manifest_url': f'{base_url.rstrip("/")}/albums/{aid}/{version}/manifest.json',
                          'genre': genre, 'artist': artist, 'title': title}


def _synchsafe(raw):
    if len(raw) != 4 or any(byte & 0x80 for byte in raw):
        raise ValueError('Cabeçalho ID3 inválido')
    return (raw[0] << 21) | (raw[1] << 14) | (raw[2] << 7) | raw[3]


def mp3_genre(path):
    """Read common ID3v2.3/v2.4 TCON fields, without loading the audio data."""
    with path.open('rb') as source:
        header = source.read(10)
        if len(header) < 10 or header[:3] != b'ID3' or header[3] not in (3, 4):
            return None
        size = _synchsafe(header[6:10])
        if size > 4 * 1024 * 1024 or header[5] & 0x80:  # oversized / unsynchronised tags
            return None
        data = source.read(size)
    version = header[3]
    if header[5] & 0x40:  # skip the extended header
        if len(data) < 4:
            return None
        ext = _synchsafe(data[:4]) if version == 4 else int.from_bytes(data[:4], 'big') + 4
        data = data[ext:]
    pos = 0
    while pos + 10 <= len(data):
        frame = data[pos:pos + 4]
        if frame == b'\0\0\0\0':
            break
        if any(byte < 0x30 or byte > 0x5a for byte in frame):
            break
        length = _synchsafe(data[pos + 4:pos + 8]) if version == 4 else int.from_bytes(data[pos + 4:pos + 8], 'big')
        pos += 10
        if length < 1 or pos + length > len(data):
            break
        if frame == b'TCON':
            payload = data[pos:pos + length]
            encoding = {0: 'latin-1', 1: 'utf-16', 2: 'utf-16-be', 3: 'utf-8'}.get(payload[0])
            if encoding:
                value = payload[1:].decode(encoding, errors='replace').split('\0', 1)[0].strip()
                return value or None
        pos += length
    return None


def load_overrides(path):
    if path is None:
        return {'albums': {}, 'artists': {}}
    config = json.loads(Path(path).read_text(encoding='utf-8'))
    if not isinstance(config, dict) or any(not isinstance(config.get(k, {}), dict) for k in ('albums', 'artists')):
        raise ValueError('O arquivo de gêneros precisa conter objetos albums e artists')
    return {'albums': config.get('albums', {}), 'artists': config.get('artists', {})}


def choose_genre(album_dir, artist, overrides, default=None, policy=None):
    key = f'{artist}/{album_dir.name}'
    configured = overrides['albums'].get(key) or overrides['artists'].get(artist)
    if configured:
        return policy.resolve(configured) if policy else valid_genre(configured)
    genres = Counter(filter(None, (mp3_genre(p) for p in sorted(album_dir.iterdir()) if p.suffix.lower() == '.mp3')))
    if genres:
        resolved = set()
        for raw in genres:
            if policy:
                resolved.add(policy.resolve(raw))
            else:
                parts = [part.strip() for part in raw.split(';') if part.strip()]
                if not parts or len({genre_key(part) for part in parts}) != 1:
                    raise GenreReviewNeeded(f'{key}: tag com gêneros misturados: {raw}')
                resolved.add(valid_genre(parts[0]))
        if len(resolved) == 1:
            return resolved.pop()
    if not genres and default:
        return policy.resolve(default) if policy else valid_genre(default)
    reason = 'não tem tags de gênero' if not genres else f'tem gêneros diferentes: {dict(genres)}'
    raise GenreReviewNeeded(f'{key}: {reason}. Selecione um gênero cadastrado.')


def discover(source, overrides, default=None, layout='artist-album', policy=None):
    source = Path(source)
    if not source.is_dir():
        raise ValueError(f'Pasta de entrada inexistente: {source}')
    jobs = []
    def children(parent):
        for child in sorted(parent.iterdir()):
            if child.name.startswith('.'):
                continue
            if not child.is_dir() or child.is_symlink():
                raise ValueError(f'Esperada pasta em {parent}: {child}')
            valid_component(child.name)
            yield child

    if layout == 'artist-album':
        artists = [(artist_dir, None) for artist_dir in children(source)]
    elif layout == 'genre-artist-album':
        artists = [(artist_dir, policy.resolve(genre_dir.name) if policy else valid_genre(genre_dir.name))
                   for genre_dir in children(source) for artist_dir in children(genre_dir)]
    else:
        raise ValueError(f'Estrutura desconhecida: {layout}')
    for artist_dir, folder_genre in artists:
        artist = artist_dir.name
        for album_dir in children(artist_dir):
            genre = folder_genre or choose_genre(album_dir, artist, overrides, default, policy)
            jobs.append((album_dir, artist, genre))
    if not jobs:
        raise ValueError('Nenhum álbum encontrado em Artista/Álbum')
    return jobs


def existing_index(base_url):
    url = base_url.rstrip('/') + '/index.json'
    req = urllib.request.Request(url, headers={'User-Agent': 'jukebox-catalog/1.0'})
    with urllib.request.urlopen(req, timeout=30) as response:
        data = response.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024:
        raise ValueError('Índice remoto excede 8 MiB')
    index = json.loads(data)
    if index.get('schema') != 1 or not isinstance(index.get('albums'), list):
        raise ValueError('Índice remoto incompatível; publicação cancelada')
    ids = set()
    for entry in index['albums']:
        aid = valid_component(entry['id'])
        if aid in ids or not entry.get('manifest_url', '').startswith('https://'):
            raise ValueError('Índice remoto inválido ou com IDs duplicados')
        ids.add(aid)
    return index


def existing_identity(entry):
    if all(entry.get(field) for field in ('artist', 'title', 'genre')):
        return entry['artist'], entry['title'], entry['genre']
    req = urllib.request.Request(entry['manifest_url'], headers={'User-Agent': 'jukebox-catalog/1.0'})
    with urllib.request.urlopen(req, timeout=30) as response:
        manifest = json.load(response)
    if manifest.get('id') != entry['id']:
        raise ValueError('Manifesto existente não corresponde ao índice')
    return manifest['artist'], manifest['title'], manifest['genre']


def prepare_metadata_edits(edits, base_url, output, policy, index=None, expected_version=None):
    """Publish new immutable manifests, retaining IDs and every media URL."""
    if not base_url.startswith('https://') or '?' in base_url or '#' in base_url:
        raise ValueError('URL pública deve usar HTTPS sem parâmetros')
    output = Path(output)
    if output.exists() and any(output.iterdir()):
        raise ValueError('Use uma pasta de saída nova e vazia')
    old = index if index is not None else existing_index(base_url)
    if expected_version is not None and old['version'] != expected_version:
        raise ValueError('O catálogo mudou desde a abertura; atualize e revise a edição')
    entries = {entry['id']: dict(entry) for entry in old['albums']}
    changes = []
    for aid, fields in edits.items():
        if aid not in entries:
            raise ValueError('Álbum não existe mais no servidor; atualize o catálogo')
        entry = entries[aid]
        req = urllib.request.Request(entry['manifest_url'], headers={'User-Agent': 'jukebox-catalog/1.0'})
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read(8 * 1024 * 1024 + 1)
        if len(raw) > 8 * 1024 * 1024:
            raise ValueError('Manifesto excede 8 MiB')
        manifest = json.loads(raw)
        if manifest.get('schema') != 1 or manifest.get('id') != aid or manifest.get('version') != entry['version']:
            raise ValueError('Manifesto remoto incompatível')
        artist = valid_component(fields['artist'].strip())
        title = valid_component(fields['title'].strip())
        genre = policy.canonical(fields['genre'])
        if (artist, title, genre) == (manifest['artist'], manifest['title'], manifest['genre']):
            continue
        updated = dict(manifest, artist=artist, title=title, genre=genre)
        updated.pop('version', None)
        version = hashlib.sha256(json.dumps(updated, ensure_ascii=False, sort_keys=True).encode()).hexdigest()[:20]
        updated['version'] = version
        release = output / 'albums' / valid_component(aid) / version
        release.mkdir(parents=True, exist_ok=True)
        (release / 'manifest.json').write_text(json.dumps(updated, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        entries[aid] = dict(entry, artist=artist, title=title, genre=genre, version=version,
                            manifest_url=f'{base_url.rstrip("/")}/albums/{aid}/{version}/manifest.json')
        changes.append((artist, title, genre, aid, version))
    identities = set()
    for entry in entries.values():
        artist, title, _ = existing_identity(entry)
        identity = (genre_key(artist), genre_key(title))
        if identity in identities:
            raise ValueError(f'Artista/álbum duplicado no catálogo: {artist} / {title}')
        identities.add(identity)
    if not changes:
        return []
    ordered = [entries[key] for key in sorted(entries)]
    version = hashlib.sha256(json.dumps(ordered, ensure_ascii=False, sort_keys=True).encode()).hexdigest()[:20]
    data = json.dumps({'schema': 1, 'version': version, 'albums': ordered}, ensure_ascii=False, indent=2) + '\n'
    if len(data.encode()) > 8 * 1024 * 1024:
        raise ValueError('Índice excede 8 MiB')
    (output / 'index.json').write_text(data, encoding='utf-8')
    (output / '.remote-version').write_text(str(old['version']), encoding='utf-8')
    return changes


def prepare(source, base_url, output, overrides, default=None, index=None, layout='artist-album', policy=None):
    if Path(source).resolve() == Path(output).resolve() or Path(source).resolve() in Path(output).resolve().parents:
        raise ValueError('Pasta de saída precisa ficar fora da pasta de entrada')
    jobs = discover(source, overrides, default, layout, policy)
    return prepare_jobs(jobs, base_url, output, index)


def prepare_jobs(jobs, base_url, output, index=None):
    if not base_url.startswith('https://') or '?' in base_url or '#' in base_url:
        raise ValueError('URL pública deve usar HTTPS sem parâmetros')
    if not jobs:
        raise ValueError('Selecione ao menos um álbum')
    incoming = set()
    for album_dir, artist, genre in jobs:
        identity = (artist.casefold(), Path(album_dir).name.casefold())
        if identity in incoming:
            raise ValueError(f'Álbum repetido na seleção: {artist} / {Path(album_dir).name}')
        incoming.add(identity)
    output = Path(output)
    if output.exists() and any(output.iterdir()):
        raise ValueError('Use uma pasta de saída nova e vazia para cada lote')
    old_index = index if index is not None else existing_index(base_url)
    old = {entry['id']: entry for entry in old_index['albums']}
    identities = {}
    for entry in old.values():
        artist, title, genre = existing_identity(entry)
        key = (artist.casefold(), title.casefold())
        if key in identities and identities[key][0] != entry['id']:
            raise ValueError(f'Catálogo remoto já contém discos duplicados: {artist} / {title}')
        identities[key] = (entry['id'], genre)
    entries = dict(old)
    changes = []
    for album_dir, artist, genre in jobs:
        identity = (artist.casefold(), album_dir.name.casefold())
        if identity in identities and identities[identity][1] != genre:
            raise ValueError(f'Gênero de {artist} / {album_dir.name} já publicado como '
                             f'{identities[identity][1]}; renomear pasta remota exige migração')
        existing_id = identities[identity][0] if identity in identities else None
        aid, version, entry = build_album(album_dir, genre, artist, base_url, output, album_id=existing_id)
        if identity in identities and identities[identity][0] != aid:
            raise ValueError(f'ID incompatível com disco existente: {artist} / {album_dir.name}')
        identities[identity] = (aid, genre)
        if aid in entries and entries[aid]['version'] == version:
            shutil.rmtree(output / 'albums' / aid / version)
            print(f'Sem mudanças: {artist} / {album_dir.name}')
            continue
        entries[aid] = entry
        changes.append((artist, album_dir.name, genre, aid, version))
    if not changes:
        return changes
    ordered = [entries[key] for key in sorted(entries)]
    version = hashlib.sha256(json.dumps(ordered, ensure_ascii=False, sort_keys=True).encode()).hexdigest()[:20]
    result = {'schema': 1, 'version': version, 'albums': ordered}
    serialized = json.dumps(result, ensure_ascii=False, indent=2) + '\n'
    if len(serialized.encode()) > 8 * 1024 * 1024:
        raise ValueError('Índice excede 8 MiB; publicação cancelada')
    output.mkdir(parents=True, exist_ok=True)
    (output / 'index.json').write_text(serialized, encoding='utf-8')
    (output / '.remote-version').write_text(str(old_index['version']), encoding='utf-8')
    return changes


def publish(output, bucket, profile, endpoint, base_url, progress=None):
    if not endpoint.startswith('https://'):
        raise ValueError('Endpoint S3 deve usar HTTPS')
    if not (Path(output) / 'index.json').is_file() or not (Path(output) / '.remote-version').is_file():
        raise ValueError('Lote preparado não encontrado')
    prepared = json.loads((Path(output) / 'index.json').read_text())
    if any(not e['manifest_url'].startswith(base_url.rstrip('/') + '/') for e in prepared['albums']):
        raise ValueError('URL pública não corresponde ao lote preparado')
    if existing_index(base_url)['version'] != (Path(output) / '.remote-version').read_text():
        raise ValueError('Índice remoto mudou desde o preparo; gere um novo lote antes de publicar')
    base = ['aws', '--profile', profile, '--endpoint-url', endpoint, 's3', 'cp']
    def upload(command):
        if progress is None:
            subprocess.run(command, check=True)
            return
        process = subprocess.Popen(command + ['--no-progress'], stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, text=True, bufsize=1)
        last_line = ''
        for line in process.stdout:
            last_line = line.rstrip()
            progress(last_line)
        if process.wait() != 0:
            raise RuntimeError(f'Envio ao R2 falhou: {last_line or "AWS CLI não retornou detalhes"}')
    # Only replace index.json after all media and album manifests uploaded.
    upload(base + [str(Path(output) / 'albums'), f's3://{bucket}/albums/', '--recursive'])
    if existing_index(base_url)['version'] != (Path(output) / '.remote-version').read_text():
        raise ValueError('Índice remoto mudou durante o envio; arquivos enviados, índice preservado')
    upload(base + [str(Path(output) / 'index.json'), f's3://{bucket}/index.json',
                   '--content-type', 'application/json', '--cache-control', 'no-cache'])


def main():
    parser = argparse.ArgumentParser(description='Prepara e opcionalmente publica vários álbuns no R2')
    parser.add_argument('source', nargs='?', help='Pasta de entrada com estrutura Artista/Álbum')
    parser.add_argument('--base-url', required=True, help='URL pública HTTPS do bucket')
    parser.add_argument('--out', required=True, help='Pasta nova e vazia para este lote')
    parser.add_argument('--genres', help='JSON opcional de gêneros por artista ou álbum')
    parser.add_argument('--genre-policy', type=Path,
                        default=Path.home() / '.config/jukebox-publisher/genres.json',
                        help='Cadastro de gêneros e sinônimos do publicador gráfico')
    parser.add_argument('--default-genre', help='Usado somente em álbuns sem tags de gênero')
    parser.add_argument('--layout', choices=('artist-album', 'genre-artist-album'),
                        default='artist-album', help='Estrutura da pasta de entrada')
    parser.add_argument('--publish', action='store_true', help='Envie ao R2 após preparar e validar o lote')
    parser.add_argument('--upload-prepared', action='store_true', help='Envia o lote já revisado em --out')
    parser.add_argument('--bucket', default='jukebox')
    parser.add_argument('--profile', default='jukebox-r2')
    parser.add_argument('--endpoint', default='https://3980df26cd4bda7486ef4764f3bd5be0.r2.cloudflarestorage.com')
    args = parser.parse_args()
    if args.upload_prepared:
        if args.publish or args.source:
            parser.error('Use --upload-prepared sem pasta de entrada e sem --publish')
        publish(args.out, args.bucket, args.profile, args.endpoint, args.base_url)
        print('Lote publicado; index.json enviado por último.')
        return
    if not args.source:
        parser.error('Informe a pasta de entrada Artista/Álbum')
    if args.genre_policy.is_file():
        data = json.loads(args.genre_policy.read_text(encoding='utf-8'))
        policy = GenrePolicy(data['genres'], data['aliases'])
    else:
        policy = GenrePolicy()
    changes = prepare(args.source, args.base_url, args.out, load_overrides(args.genres),
                      args.default_genre, layout=args.layout, policy=policy)
    for artist, title, genre, aid, version in changes:
        print(f'{genre} / {artist} / {title} — {aid} versão {version}')
    print(f'{len(changes)} álbum(ns) novo(s)/alterado(s). Índice remoto preservado.')
    if args.publish and changes:
        publish(args.out, args.bucket, args.profile, args.endpoint, args.base_url)
        print('Lote publicado; index.json enviado por último.')


if __name__ == '__main__':
    main()
