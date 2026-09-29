#!/usr/bin/env python3
"""Remove catalog entries for confirmed missing files. Close the jukebox first."""
import argparse
from datetime import datetime, timezone
import os
from pathlib import Path
import sqlite3


def repair(database, media_root, working_dir=None):
    database = Path(database).resolve()
    media_root = Path(media_root).resolve()
    working_dir = Path(working_dir).resolve() if working_dir else media_root.parent.parent
    if not database.is_file():
        raise ValueError(f'Banco não encontrado: {database}')
    if not media_root.is_dir():
        raise ValueError(f'Pasta de músicas indisponível: {media_root}; banco preservado')
    # Do not interpret an unreadable/unmounted root as an empty collection.
    list(media_root.iterdir())
    connection = sqlite3.connect(database.as_uri() + '?mode=rw', uri=True, timeout=10)
    try:
        paths = [row[0] for row in connection.execute('SELECT file_path FROM tracks')]
        missing = []
        for stored in paths:
            candidate = Path(stored)
            if not candidate.is_absolute():
                candidate = working_dir / candidate
            candidate = Path(os.path.abspath(candidate))
            if not candidate.is_relative_to(media_root):
                continue
            try:
                candidate.stat()
            except FileNotFoundError:
                missing.append(stored)
            except OSError:
                pass  # Permission / IO errors do not prove that media was deleted.
        if not missing:
            return 0, None
        stamp = datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S-%f')
        backup = database.with_name(database.name + f'.backup-{stamp}')
        with sqlite3.connect(backup) as target:
            connection.backup(target)
        fingerprints = connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='media_fingerprints'").fetchone()
        with connection:
            for stored in missing:
                if fingerprints:
                    connection.execute('DELETE FROM media_fingerprints WHERE file_path=?', (stored,))
                connection.execute('DELETE FROM tracks WHERE file_path=?', (stored,))
        return len(missing), backup
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database', type=Path, help='Banco jukebox.db existente')
    parser.add_argument('--media-root', type=Path, help='Pasta musicas; padrão: junto ao banco')
    parser.add_argument('--working-dir', type=Path, help='Pasta de execução para caminhos relativos')
    args = parser.parse_args()
    try:
        count, backup = repair(args.database, args.media_root or args.database.resolve().parent / 'musicas', args.working_dir)
    except (ValueError, OSError, sqlite3.Error) as error:
        raise SystemExit(f'Não foi possível reparar o catálogo: {error}')
    print(f'Removidas {count} entradas de arquivos ausentes. Créditos e fila preservados.')
    if backup:
        print(f'Backup do banco: {backup}')
    print('Abra novamente a jukebox para reler o catálogo.')


if __name__ == '__main__':
    main()
