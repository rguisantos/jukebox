#!/usr/bin/env python3
"""Machine-local album removal; same lock as catalog and USB synchronization."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import uuid

spec = importlib.util.spec_from_file_location('storage_catalog', Path(__file__).with_name('catalog_sync.py'))
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)
MEDIA = {'.mp3', '.mp4', '.wav', '.wmv', '.mpeg'}


def marker_id(folder):
    for name in ('.jukebox-album.json', '.jukebox-download.json'):
        marker = folder / name
        if marker.is_file() and not marker.is_symlink():
            value = json.loads(marker.read_text()).get('id')
            if value:
                return catalog.component(value)
    return None


def copies(root, aid):
    downloads = root / '.downloads'
    if downloads.is_symlink():
        raise ValueError('Pasta de downloads não pode ser um link')
    if not downloads.is_dir():
        return []
    result = []
    for folder in downloads.iterdir():
        if folder.is_dir() and not folder.is_symlink():
            try:
                if marker_id(folder) == aid:
                    result.append(folder)
            except (ValueError, OSError):
                continue  # Never infer ownership of unknown/old partial folders.
    return result


def size_of(folders):
    seen, total = set(), 0
    for folder in folders:
        for current, dirs, files in os.walk(folder, followlinks=False):
            dirs[:] = [d for d in dirs if not (Path(current) / d).is_symlink()]
            for name in files:
                path = Path(current) / name
                if path.is_symlink():
                    continue
                stat = path.stat()
                key = (stat.st_dev, stat.st_ino)
                if key not in seen:
                    total += stat.st_size
                    seen.add(key)
    return total


def list_albums(root, exclusions):
    media = root / 'musicas'
    if not media.is_dir() or media.is_symlink():
        raise ValueError('Pasta de músicas indisponível')
    rows = {}
    def walk_error(error):
        raise error
    for current, dirs, files in os.walk(media, followlinks=False, onerror=walk_error):
        folder = Path(current)
        dirs[:] = sorted(d for d in dirs if not d.startswith('.') and not (folder / d).is_symlink()
                         and not (folder == media and d.casefold() == 'fundos'))
        if folder == media or not any(Path(name).suffix.lower() in MEDIA for name in files):
            continue
        relative = folder.relative_to(root)
        aid = marker_id(folder)
        online = aid is not None
        aid = aid or 'local-' + hashlib.sha256(str(relative).encode()).hexdigest()[:20]
        if aid in rows:
            raise ValueError('Álbum duplicado no disco; corrija o catálogo antes de remover')
        parts = folder.relative_to(media).parts
        rows[aid] = dict(id=aid, path=str(relative), artist=parts[-2] if len(parts) >= 2 else 'Vários Artistas',
                         title=parts[-1], genre=parts[-3] if len(parts) >= 3 else 'Desconhecido',
                         size=size_of([folder] + (copies(root, aid) if online else [])), online=online, removed=False)
    for aid, album in exclusions.items():
        if aid not in rows:
            rows[aid] = dict(album, removed=True)
    return sorted(rows.values(), key=lambda row: (row['removed'], row['artist'].casefold(), row['title'].casefold()))


def operate(root, request):
    root = Path(root).resolve()
    if not root.is_dir():
        raise ValueError('Diretório de dados indisponível')
    with (root / '.catalog-sync.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        exclusions = catalog.load_exclusions(root)
        rows = list_albums(root, exclusions)
        action = request.get('action', 'list')
        changed, restore, message = False, False, 'Selecione um álbum ou Voltar.'
        if action != 'list':
            if action not in ('remove', 'restore'):
                raise ValueError('Operação desconhecida')
            selected = request.get('album', {})
            matches = [row for row in rows if row['id'] == selected.get('id') and row['path'] == selected.get('path')]
            if len(matches) != 1:
                raise ValueError('O álbum mudou; atualize a lista antes de continuar')
            album = matches[0]
            aid = album['id']
            if action == 'restore':
                if not album['removed'] or not album['online']:
                    raise ValueError('Álbum USB precisa ser importado novamente pelo pendrive')
                exclusions.pop(aid)
                catalog.atomic_json(root / 'catalog-exclusions.json', {'schema': 1, 'albums': exclusions})
                restore, message = True, 'Restauração liberada; download solicitado ao servidor.'
            else:
                if album['removed']:
                    raise ValueError('Este álbum já foi removido')
                relative = Path(album['path'])
                if relative.is_absolute() or relative.parts[0] != 'musicas' or '..' in relative.parts:
                    raise ValueError('Caminho de álbum inválido')
                folder = root / relative
                if folder.is_symlink() or not folder.resolve().is_relative_to(root / 'musicas'):
                    raise ValueError('Caminho não permitido')
                if any(not row['removed'] and row['id'] != aid
                       and (root / row['path']).is_relative_to(folder) for row in rows):
                    raise ValueError('Esta pasta contém outros álbuns; remova os álbuns internos primeiro.')
                protected = request.get('protected_paths')
                if not isinstance(protected, list):
                    raise ValueError('Não foi possível conferir a fila; remoção bloqueada')
                for stored in protected:
                    path = Path(stored)
                    if not path.is_absolute():
                        path = Path(request.get('working_dir', os.getcwd())) / path
                    resolved = path.resolve()
                    if resolved.is_relative_to(folder.resolve()):
                        raise ValueError('Álbum tocando ou com músicas na fila. Aguarde terminar.')
                    if resolved.is_relative_to(root / 'musicas') and not path.is_file():
                        raise ValueError('A fila contém arquivo indisponível; aguarde terminar antes de remover álbuns.')
                backups = copies(root, aid) if album['online'] else []
                if album['online']:
                    exclusions[aid] = dict(album, removed=True)
                    # Persist before removing files so a reboot cannot re-enable downloads.
                    catalog.atomic_json(root / 'catalog-exclusions.json', {'schema': 1, 'albums': exclusions})
                trash_root = root / '.downloads'
                if trash_root.is_symlink():
                    raise ValueError('Pasta de downloads não pode ser um link')
                trash_root.mkdir(exist_ok=True)
                trash = trash_root / ('removed-' + uuid.uuid4().hex)
                os.rename(folder, trash)
                catalog.sync_dir(folder.parent)
                catalog.sync_dir(trash_root)
                shutil.rmtree(trash)
                for backup in backups:
                    shutil.rmtree(backup)
                catalog.sync_dir(trash_root)
                changed = True
                message = 'Álbum removido desta máquina; outras máquinas e servidor preservados.'
            rows = list_albums(root, exclusions)
        return dict(rows=rows, changed=changed, restore=restore, message=message)


def main():
    try:
        request = json.load(sys.stdin)
        root = Path(os.environ.get('JUKEBOX_DATA_DIR', '/dados'))
        result = operate(root, request)
        print(json.dumps(result, ensure_ascii=False))
    except Exception as error:
        print(json.dumps({'error': str(error)}, ensure_ascii=False))
        sys.exit(1)


if __name__ == '__main__':
    main()
