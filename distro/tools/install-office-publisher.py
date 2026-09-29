#!/usr/bin/env python3
"""Add the office publisher to the current user's application menu."""
from pathlib import Path

try:
    import tkinter
except ImportError:
    raise SystemExit('Tkinter não está instalado. Em Ubuntu/Debian execute: sudo apt install python3-tk')

gui = Path(__file__).with_name('jukebox-publisher-gui.py').resolve()
engine = gui.with_name('publish-r2-catalog.py')
if not gui.is_file() or not engine.is_file():
    raise SystemExit('Mantenha os dois arquivos na mesma pasta antes de instalar.')
if any(char in str(gui) for char in ('\n', '\r', '"', '\\')):
    raise SystemExit('Caminho do programa contém caracteres não suportados pelo atalho.')

target = Path.home() / '.local/share/applications/jukebox-publisher.desktop'
target.parent.mkdir(parents=True, exist_ok=True)
target.write_text(f'''[Desktop Entry]
Type=Application
Name=Jukebox - Publicador de Álbuns
Comment=Organize álbuns e publique no servidor R2
Exec=/usr/bin/env python3 "{gui}"
Icon=audio-x-generic
Terminal=false
Categories=AudioVideo;Audio;
''', encoding='utf-8')
print(f'Atalho instalado: {target}')
