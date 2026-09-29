#!/usr/bin/env python3
"""Incremental HTTPS catalog client. No access to operational SQLite tables."""
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import time
import urllib.request

LIMIT = 8 * 1024 * 1024
EXTENSIONS = {'.mp3', '.mp4', '.wav', '.wmv', '.mpeg', '.jpg', '.jpeg', '.png', '.webp'}


def emit(message, **fields):
    print(json.dumps(dict(message=message, **fields), ensure_ascii=False), flush=True)


def component(value):
    if not isinstance(value, str) or not value or value in {'.', '..'} or len(value.encode()) > 180:
        raise ValueError('Nome de arquivo/pasta inválido')
    if any(c in value for c in '/\\\x00') or any(ord(c) < 32 for c in value) or value.startswith('.'):
        raise ValueError('Caminho não permitido')
    return value


class HTTPSOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.startswith('https://'):
            raise ValueError('Redirecionamento precisa de HTTPS')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


OPENER = urllib.request.build_opener(HTTPSOnly())


def request(url, headers=None):
    if not isinstance(url, str) or not url.startswith('https://'):
        raise ValueError('O servidor deve usar HTTPS')
    # Cloudflare's public R2 endpoint may reject urllib's default Python-urllib
    # User-Agent with HTTP 403 while serving the same public object to curl.
    request_headers = {'User-Agent': 'jukebox-catalog/1.0'}
    request_headers.update(headers or {})
    return OPENER.open(urllib.request.Request(url, headers=request_headers), timeout=30)


def fetch_json(url):
    with request(url) as response:
        data = response.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError('Manifesto excede 8 MiB; divida o acervo por álbum')
    return json.loads(data)


def atomic_json(path, data):
    tmp = path.with_suffix('.tmp')
    with tmp.open('w') as out:
        json.dump(data, out, ensure_ascii=False)
        out.flush()
        os.fsync(out.fileno())
    os.replace(tmp, path)
    sync_dir(path.parent)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as src:
        for chunk in iter(lambda: src.read(256 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def album_complete(marker_path, aid, version):
    """Fast stat check for unchanged albums; hash only files whose metadata changed."""
    try:
        marker = json.loads(marker_path.read_text())
        files = marker.get('files')
        if marker.get('id') != aid or marker.get('version') != version or not isinstance(files, list) or not files:
            return False  # Older markers are upgraded on the next sync.
        for item in files:
            path = marker_path.parent / component(item['path'])
            stat = path.stat()
            if not path.is_file() or stat.st_size != item['size']:
                return False
            if stat.st_mtime_ns != item['mtime_ns'] and digest(path) != item['sha256']:
                return False
        return True
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError):
        return False


def download(item, target, kib):
    size, checksum = item['size'], item['sha256']
    if type(size) is not int or size <= 0 or not re.fullmatch('[0-9a-f]{64}', checksum):
        raise ValueError('Tamanho ou SHA-256 inválido')
    partial = target.with_name(target.name + '.part')
    # Include hash in staging directory so resumed bytes belong to this exact version.
    offset = partial.stat().st_size if partial.exists() else 0
    if offset > size:
        partial.unlink()
        offset = 0
    if offset == size:
        if digest(partial) == checksum:
            os.replace(partial, target)
            return
        partial.unlink()
        offset = 0
    if shutil.disk_usage(target.parent).free < size - offset + 128 * 1024 * 1024:
        raise OSError('Espaço insuficiente; reservados 128 MiB para operação')
    headers = {'Accept-Encoding': 'identity'}
    if offset:
        headers['Range'] = f'bytes={offset}-'
    with request(item['url'], headers) as response:
        if offset and response.status == 206:
            expected = f'bytes {offset}-{size-1}/{size}'
            if response.headers.get('Content-Range') != expected:
                raise ValueError('Servidor devolveu intervalo incorreto')
        elif response.status == 200:
            offset = 0  # Server does not implement ranges; safely restart.
        else:
            raise ValueError('Resposta de download inesperada')
        total = offset
        began = time.monotonic()
        downloaded = 0
        last_progress = 0
        with partial.open('ab' if offset else 'wb') as out:
            while True:
                chunk = response.read(64 * 1024)
                if not chunk:
                    break
                total += len(chunk)
                if total > size:
                    raise ValueError('Arquivo maior que o manifesto')
                out.write(chunk)
                downloaded += len(chunk)
                if kib > 0:
                    delay = downloaded / (kib * 1024) - (time.monotonic() - began)
                    if delay > 0:
                        time.sleep(delay)
                if time.monotonic() - last_progress >= 1:
                    emit(f'Baixando {target.name}: {total * 100 // size}%', downloaded=total, size=size)
                    last_progress = time.monotonic()
            out.flush()
            os.fsync(out.fileno())
    if total != size:
        raise OSError('Download interrompido; será retomado')
    if digest(partial) != checksum:
        partial.unlink()
        raise ValueError('SHA-256 divergente; arquivo descartado')
    os.replace(partial, target)
    sync_dir(target.parent)


def publish(stage, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not destination.exists():
        os.rename(stage, destination)
    else:
        # Atomic directory exchange: readers see the complete old OR new album.
        libc = ctypes.CDLL(None, use_errno=True)
        rename = libc.renameat2
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        if rename(-100, os.fsencode(stage), -100, os.fsencode(destination), 2) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        # Old album is retained until a later successful sync cleanup/manual maintenance.
    sync_dir(destination.parent)
    sync_dir(stage.parent)


def sync(root, url, kib=512):
    root.mkdir(parents=True, exist_ok=True)
    index = fetch_json(url)
    if index.get('schema') != 1 or not isinstance(index.get('albums'), list):
        raise ValueError('Versão de protocolo desconhecida')
    state_path = root / 'catalog-state.json'
    state = json.loads(state_path.read_text()) if state_path.exists() else {'albums': {}}
    changed = False
    seen = set()
    for entry in index['albums']:
        aid = component(entry['id'])
        version = component(entry['version'])
        if aid in seen:
            raise ValueError('ID de álbum duplicado')
        seen.add(aid)
        old = state['albums'].get(aid, {})
        if old.get('version') == version and album_complete(root / old.get('path', '') / '.jukebox-album.json', aid, version):
            continue
        album = fetch_json(entry['manifest_url'])
        if album.get('schema') != 1 or album.get('id') != aid or album.get('version') != version:
            raise ValueError('Manifesto do álbum não corresponde ao índice')
        parts = [component(album[k]) for k in ('genre', 'artist', 'title')]
        relative = Path('musicas', *parts)
        destination = root / relative
        reuse_directory = destination
        if destination.exists():
            marker = destination / '.jukebox-album.json'
            if not marker.is_file() or json.loads(marker.read_text()).get('id') != aid:
                raise ValueError('Destino pertence a outro álbum ou ao acervo USB')
        files = album['files']
        if not files or not isinstance(files, list):
            raise ValueError('Álbum vazio')
        names = set()
        for item in files:
            name = component(item['path'])
            if Path(name).suffix.lower() not in EXTENSIONS or name in names:
                raise ValueError('Arquivo inválido ou repetido')
            if type(item.get('size')) is not int or item['size'] <= 0 or not isinstance(item.get('sha256'), str) or not re.fullmatch('[0-9a-f]{64}', item['sha256']):
                raise ValueError('Tamanho ou SHA-256 inválido')
            names.add(name)
        if not any(Path(name).suffix.lower() in {'.mp3', '.mp4', '.wav', '.wmv', '.mpeg'} for name in names):
            raise ValueError('Álbum deve conter ao menos uma faixa')
        if old and old.get('path') != str(relative):
            old_parts = Path(old['path']).parts
            if len(old_parts) != 4 or old_parts[0] != 'musicas':
                raise ValueError('Caminho anterior do álbum inválido')
            source = root.joinpath(*(component(part) for part in old_parts))
            # Recover if the process stopped after rename but before saving state.
            if not source.exists() and destination.exists():
                source = destination
            marker = source / '.jukebox-album.json'
            if source.is_symlink() or (source.exists() and
                    (not marker.is_file() or json.loads(marker.read_text()).get('id') != aid)):
                raise ValueError('Álbum anterior não encontrado; acervo local preservado')
            reuse_directory = source
            intact = all((source / item['path']).is_file()
                         and (source / item['path']).stat().st_size == item['size']
                         and digest(source / item['path']) == item['sha256'] for item in files)
            if intact and (source == destination or not destination.exists()):
                verified = [dict(path=item['path'], size=item['size'], sha256=item['sha256'],
                                 mtime_ns=(source / item['path']).stat().st_mtime_ns) for item in files]
                atomic_json(marker, {'id': aid, 'version': version, 'files': verified})
                if source != destination:
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    os.rename(source, destination)
                    sync_dir(source.parent)
                    sync_dir(destination.parent)
                state['albums'][aid] = {'version': version, 'path': str(relative)}
                atomic_json(state_path, state)
                changed = True
                for cache in (root / 'capas').glob('*.jbc'):
                    cache.unlink(missing_ok=True)
                emit(f'Álbum reorganizado: {album["title"]}', changed=True)
                continue
        # Upgrade markers from older installs without swapping thousands of intact albums.
        marker = destination / '.jukebox-album.json'
        if old.get('version') == version and marker.is_file():
            metadata = json.loads(marker.read_text())
            if metadata.get('id') == aid and metadata.get('version') == version and 'files' not in metadata:
                if all((destination / item['path']).is_file()
                       and (destination / item['path']).stat().st_size == item['size'] for item in files):
                    metadata['files'] = [dict(path=item['path'], size=item['size'], sha256=item['sha256'],
                                              mtime_ns=(destination / item['path']).stat().st_mtime_ns) for item in files]
                    atomic_json(marker, metadata)
                    continue
        # Hash the complete manifest so changed URLs/contents cannot reuse unrelated partials.
        token = hashlib.sha256(json.dumps(album, sort_keys=True).encode()).hexdigest()
        stage = root / '.downloads' / token
        stage.mkdir(parents=True, exist_ok=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        if stage.stat().st_dev != destination.parent.stat().st_dev:
            raise ValueError('Downloads e acervo precisam estar na mesma partição')
        for item in files:
            name = item['path']
            target = stage / name
            if target.is_file() and target.stat().st_size == item['size'] and digest(target) == item['sha256']:
                continue
            existing = reuse_directory / name
            if existing.is_file() and existing.stat().st_size == item['size'] and digest(existing) == item['sha256']:
                if target.exists():
                    target.unlink()
                os.link(existing, target)
            else:
                emit(f'Atualizando {album["title"]}: {name}')
                download(item, target, 0 if kib == 0 else max(16, kib))
        # An omitted track is not an instruction to delete local media.
        if reuse_directory.exists():
            for source in reuse_directory.iterdir():
                if source.is_file() and not source.name.startswith('.') and source.name not in names:
                    target = stage / source.name
                    if not target.exists():
                        os.link(source, target)
        verified_files = [dict(path=item['path'], size=item['size'], sha256=item['sha256'],
                               mtime_ns=(stage / item['path']).stat().st_mtime_ns) for item in files]
        atomic_json(stage / '.jukebox-album.json', {'id': aid, 'version': version, 'files': verified_files})
        publish(stage, destination)
        if reuse_directory != destination and reuse_directory.exists():
            retired = root / '.downloads' / f'retired-{aid}-{time.time_ns()}'
            os.rename(reuse_directory, retired)
            sync_dir(reuse_directory.parent)
            sync_dir(retired.parent)
        state['albums'][aid] = {'version': version, 'path': str(relative)}
        atomic_json(state_path, state)
        changed = True
        # Existing cover cache is keyed by artist/album, not remote version.
        for cache in (root / 'capas').glob('*.jbc'):
            cache.unlink(missing_ok=True)
        emit(f'Álbum disponível: {album["title"]}', changed=True)
    emit('Acervo atualizado', complete=True, changed=changed)
    return changed


def main():
    root = Path(os.environ.get('JUKEBOX_DATA_DIR', '/dados'))
    url = os.environ.get('JUKEBOX_CATALOG_URL', '').strip()
    if not url:
        emit('Servidor de acervo não configurado', complete=True, changed=False)
        return
    root.mkdir(parents=True, exist_ok=True)
    with (root / '.catalog-sync.lock').open('w') as lock:
        # USB imports use the same lock. Wait so a skipped check does not delay
        # a new album until the next periodic poll.
        fcntl.flock(lock, fcntl.LOCK_EX)
        sync(root, url, int(os.environ.get('JUKEBOX_DOWNLOAD_KIB', '0')))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        emit(f'Atualização adiada: {error}', error=True)
        sys.exit(1)
