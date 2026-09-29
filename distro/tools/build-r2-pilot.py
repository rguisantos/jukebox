#!/usr/bin/env python3
"""Prepare one immutable album release for the jukebox catalog v1 pilot.

No cloud credentials or dependencies are required. Upload the output to an R2
bucket, with index.json uploaded last. See distro/ONLINE-CATALOG.md.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
from urllib.parse import quote

MEDIA = {'.mp3', '.mp4', '.wav', '.wmv', '.mpeg'}
COVERS = {'.jpg', '.jpeg', '.png', '.webp'}


def valid_component(value):
    if (not value or value in {'.', '..'} or value.startswith('.')
            or len(value.encode('utf-8')) > 180
            or any(c in value for c in '/\\\x00')
            or any(ord(c) < 32 for c in value)):
        raise ValueError(f'Nome inválido: {value!r}')
    return value


def build_album(album_dir, genre, artist, base_url, output):
    album_dir = Path(album_dir)
    output = Path(output)
    if not album_dir.is_dir() or album_dir.is_symlink():
        raise ValueError('A pasta do álbum deve existir e não pode ser um link')
    title = valid_component(album_dir.name)
    genre, artist = valid_component(genre), valid_component(artist)
    if ';' in genre:
        raise ValueError('Escolha um único gênero; ponto e vírgula não é permitido')
    if not base_url.startswith('https://') or '?' in base_url or '#' in base_url:
        raise ValueError('A URL pública deve começar com https:// e não ter parâmetros')
    base_url = base_url.rstrip('/')
    if output.resolve() == album_dir.resolve() or album_dir.resolve() in output.resolve().parents:
        raise ValueError('A saída precisa estar fora da pasta do álbum')
    files = sorted(album_dir.iterdir())
    if not files or any(not p.is_file() or p.is_symlink() for p in files):
        raise ValueError('A pasta deve conter somente arquivos comuns, sem subpastas ou links')
    if any(p.suffix.lower() not in MEDIA | COVERS for p in files):
        raise ValueError('Formato não aceito no álbum; use MP3/MP4 e capas JPG/PNG/WebP')
    if not any(p.suffix.lower() in MEDIA for p in files):
        raise ValueError('O álbum precisa conter ao menos uma faixa')
    if len({p.name.casefold() for p in files}) != len(files):
        raise ValueError('Nomes de arquivos conflitantes')
    for p in files:
        valid_component(p.name)

    # The ID is stable for this genre/artist/title; the version is content-based.
    aid = 'album-' + hashlib.sha256('\0'.join([genre, artist, title]).encode()).hexdigest()[:20]
    content = []
    release_hash = hashlib.sha256()
    for p in files:
        digest = hashlib.sha256()
        with p.open('rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
        size = p.stat().st_size
        if not size:
            raise ValueError(f'Arquivo vazio: {p.name}')
        release_hash.update(json.dumps([p.name, size, digest.hexdigest()], ensure_ascii=False).encode())
        content.append((p, size, digest.hexdigest()))
    version = release_hash.hexdigest()[:20]
    release = output / 'albums' / aid / version
    release.mkdir(parents=True, exist_ok=True)
    items = []
    for p, size, checksum in content:
        target = release / p.name
        if target.exists():
            if target.stat().st_size != size or hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
                raise ValueError(f'Arquivo publicado diverge: {target}')
        else:
            shutil.copyfile(p, target)
        items.append({'path': p.name, 'url': f'{base_url}/albums/{aid}/{version}/{quote(p.name)}',
                      'size': size, 'sha256': checksum})
    manifest = {'schema': 1, 'id': aid, 'version': version, 'genre': genre,
                'artist': artist, 'title': title, 'files': items}
    manifest_path = release / 'manifest.json'
    serialized = json.dumps(manifest, ensure_ascii=False, indent=2) + '\n'
    if manifest_path.exists() and manifest_path.read_text() != serialized:
        raise ValueError('Manifesto da versão publicada diverge')
    manifest_path.write_text(serialized)
    entry = {'id': aid, 'version': version,
             'manifest_url': f'{base_url}/albums/{aid}/{version}/manifest.json',
             'genre': genre, 'artist': artist, 'title': title}
    return aid, version, entry


def build(album_dir, genre, artist, base_url, output):
    aid, version, entry = build_album(album_dir, genre, artist, base_url, output)
    index = {'schema': 1, 'version': version, 'albums': [entry]}
    (output / 'index.json').write_text(json.dumps(index, ensure_ascii=False, indent=2) + '\n')
    return aid, version


def main():
    parser = argparse.ArgumentParser(description='Prepara um álbum para teste no R2')
    parser.add_argument('album', help='Pasta contendo as faixas de um álbum')
    parser.add_argument('--genre', required=True)
    parser.add_argument('--artist', required=True)
    parser.add_argument('--base-url', required=True, help='URL pública HTTPS do bucket (domínio próprio)')
    parser.add_argument('--out', required=True, help='Pasta de saída temporária')
    args = parser.parse_args()
    aid, version = build(args.album, args.genre, args.artist, args.base_url, args.out)
    print(f'Álbum preparado: {aid} versão {version}. Envie os arquivos e o manifesto antes do index.json.')


if __name__ == '__main__':
    main()
