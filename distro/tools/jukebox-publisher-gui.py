#!/usr/bin/env python3
"""Office desktop publisher for the jukebox R2 catalog. No cloud secrets stored."""
from dataclasses import dataclass
import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import tempfile
import threading
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

ENGINE = Path(__file__).with_name('publish-r2-catalog.py')
spec = importlib.util.spec_from_file_location('jukebox_catalog_publisher', ENGINE)
engine = importlib.util.module_from_spec(spec)
spec.loader.exec_module(engine)

SETTINGS_FILE = Path.home() / '.config/jukebox-publisher/settings.json'
DRAFTS_FILE = Path.home() / '.config/jukebox-publisher/drafts.json'
GENRES_FILE = Path.home() / '.config/jukebox-publisher/genres.json'
DEFAULTS = {
    'base_url': 'https://pub-eb5579cf5dab4ba28fcdbf8d5da91bc0.r2.dev',
    'bucket': 'jukebox',
    'profile': 'jukebox-r2',
    'endpoint': 'https://3980df26cd4bda7486ef4764f3bd5be0.r2.cloudflarestorage.com',
}


@dataclass
class AlbumDraft:
    path: Path
    artist: str
    title: str
    genre: str
    tracks: int
    size: int
    note: str = ''
    blocked: bool = False


def scan_folder(folder, policy=None):
    """Accept selecting one album, one artist, or a Deemix collection root."""
    found = []
    for current, dirs, filenames in os.walk(folder, followlinks=False):
        dirs[:] = sorted(d for d in dirs if not d.startswith('.') and not (Path(current) / d).is_symlink())
        path = Path(current)
        media = [p for p in (path / name for name in filenames)
                 if p.suffix.lower() in engine.MEDIA and not p.is_symlink()]
        if not media:
            continue
        nested = [name for name in os.listdir(path) if (path / name).is_dir()]
        dirs[:] = []  # one physical folder is one jukebox album
        artist, title = path.parent.name, path.name
        try:
            engine.valid_component(artist)
            engine.valid_component(title)
            invalid = [name for name in filenames if name.startswith('.') or
                       (path / name).is_symlink() or (path / name).suffix.lower() not in engine.MEDIA | engine.COVERS]
            invalid.extend(nested)
            if invalid:
                raise ValueError('Arquivos não aceitos: ' + ', '.join(invalid[:3]))
            genre = engine.choose_genre(path, artist, {'albums': {}, 'artists': {}}, policy=policy)
            note = ''
            blocked = False
        except (ValueError, OSError) as error:
            genre = ''
            note = str(error)
            blocked = not isinstance(error, engine.GenreReviewNeeded)
        found.append(AlbumDraft(path, artist, title, genre, len(media), sum(p.stat().st_size for p in media), note, blocked))
    if not found:
        raise ValueError('Nenhuma pasta com MP3/MP4 encontrada')
    return found


def load_settings():
    if not SETTINGS_FILE.is_file():
        return DEFAULTS.copy()
    data = json.loads(SETTINGS_FILE.read_text(encoding='utf-8'))
    return {key: data.get(key, value) for key, value in DEFAULTS.items()}


def save_settings(settings):
    if not settings['base_url'].startswith('https://') or not settings['endpoint'].startswith('https://'):
        raise ValueError('As URLs do servidor devem começar com https://')
    if not all(settings.values()):
        raise ValueError('Preencha todas as configurações do servidor')
    SETTINGS_FILE.parent.mkdir(parents=True, exist_ok=True)
    temp = SETTINGS_FILE.with_suffix('.tmp')
    with temp.open('w', encoding='utf-8') as out:
        json.dump(settings, out, indent=2)
    os.chmod(temp, 0o600)
    os.replace(temp, SETTINGS_FILE)


def load_policy():
    if not GENRES_FILE.is_file():
        return engine.GenrePolicy()
    data = json.loads(GENRES_FILE.read_text(encoding='utf-8'))
    if not isinstance(data, dict) or not isinstance(data.get('genres'), list) or not isinstance(data.get('aliases'), dict):
        raise ValueError('Cadastro de gêneros inválido')
    return engine.GenrePolicy(data['genres'], data['aliases'])


def save_policy(policy):
    GENRES_FILE.parent.mkdir(parents=True, exist_ok=True)
    temp = GENRES_FILE.with_suffix('.tmp')
    with temp.open('w', encoding='utf-8') as out:
        json.dump(policy.as_json(), out, ensure_ascii=False, indent=2)
    os.chmod(temp, 0o600)
    os.replace(temp, GENRES_FILE)


def normalize_drafts(albums, policy):
    for album in albums.values():
        if album.genre:
            try:
                album.genre = policy.resolve(album.genre)
            except engine.GenreReviewNeeded:
                album.note = f'Gênero antigo não cadastrado: {album.genre}. Escolha um gênero da lista.'
                album.genre = ''
    return albums


def load_drafts():
    if not DRAFTS_FILE.is_file():
        return {}
    result = {}
    for data in json.loads(DRAFTS_FILE.read_text(encoding='utf-8')):
        path = Path(data['path'])
        if path.is_dir():
            album = AlbumDraft(path, data['artist'], data['title'], data['genre'],
                               data['tracks'], data['size'], data.get('note', ''), data.get('blocked', False))
            result[str(path.resolve())] = album
    return result


def save_drafts(albums):
    DRAFTS_FILE.parent.mkdir(parents=True, exist_ok=True)
    temp = DRAFTS_FILE.with_suffix('.tmp')
    data = [dict(path=str(a.path), artist=a.artist, title=a.title, genre=a.genre,
                 tracks=a.tracks, size=a.size, note=a.note, blocked=a.blocked)
            for a in albums.values()]
    with temp.open('w', encoding='utf-8') as out:
        json.dump(data, out, ensure_ascii=False, indent=2)
    os.chmod(temp, 0o600)
    os.replace(temp, DRAFTS_FILE)


class OfficePublisher(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title('Jukebox · Publicador de Álbuns · Gêneros cadastrados')
        self.geometry('1120x740')
        self.minsize(850, 560)
        self.settings = load_settings()
        self.policy = load_policy()
        self.remote_index = None
        self.albums = normalize_drafts(load_drafts(), self.policy)  # canonical filesystem path -> AlbumDraft
        self.item_paths = {}
        self.published = set()
        self.unchanged = set()
        self.events = queue.Queue()
        self.busy = False
        self._style()
        self._build()
        self._render()
        self.protocol('WM_DELETE_WINDOW', self._close)
        self.after(100, self._poll)
        self._refresh_catalog()

    def _style(self):
        style = ttk.Style(self)
        style.theme_use('clam')
        self.configure(bg='#edf2f7')
        style.configure('TFrame', background='#edf2f7')
        style.configure('TLabel', background='#edf2f7', foreground='#26364a', font=('Sans', 10))
        style.configure('Title.TLabel', font=('Sans', 19, 'bold'), foreground='#15283c')
        style.configure('TButton', padding=(12, 8), font=('Sans', 10))
        style.configure('Primary.TButton', background='#245bb2', foreground='white')
        style.map('Primary.TButton', background=[('active', '#1d4c98'), ('disabled', '#9caec5')])
        style.configure('Treeview', rowheight=29, font=('Sans', 10))
        style.configure('Treeview.Heading', font=('Sans', 10, 'bold'))

    def _build(self):
        root = ttk.Frame(self, padding=20)
        root.pack(fill='both', expand=True)
        header = ttk.Frame(root)
        header.pack(fill='x')
        ttk.Label(header, text='Publicador de álbuns', style='Title.TLabel').pack(side='left')
        self.server_label = ttk.Label(header, text='R2 · verificando catálogo...')
        self.server_label.pack(side='right', padx=(0, 12))
        ttk.Button(header, text='Configurar servidor', command=self._settings_dialog).pack(side='right')

        ttk.Label(root, text='Adicione uma pasta de álbum, artista ou a pasta de downloads do Deemix. Revise o gênero antes de enviar.').pack(anchor='w', pady=(10, 12))
        actions = ttk.Frame(root)
        actions.pack(fill='x', pady=(0, 10))
        self.add_btn = ttk.Button(actions, text='Adicionar pasta…', command=self._add_folder, style='Primary.TButton')
        self.add_btn.pack(side='left')
        ttk.Button(actions, text='Remover seleção', command=self._remove_selected).pack(side='left', padx=8)
        ttk.Button(actions, text='Atualizar catálogo', command=self._refresh_catalog).pack(side='right')

        server_actions = ttk.Frame(root)
        server_actions.pack(fill='x', pady=(0, 10))
        ttk.Button(server_actions, text='Acervo do servidor…', command=self._remote_dialog).pack(side='left')
        self.genres_btn = ttk.Button(server_actions, text='Gerenciar gêneros…', command=self._genres_dialog)
        self.genres_btn.pack(side='left', padx=8)

        table = ttk.Frame(root)
        table.pack(fill='both', expand=True)
        self.tree = ttk.Treeview(table, columns=('genre', 'tracks', 'size', 'state'), show='tree headings', selectmode='extended')
        self.tree.heading('#0', text='ARTISTA  >  ÁLBUM')
        for col, title in [('genre', 'GÊNERO'), ('tracks', 'FAIXAS'), ('size', 'TAMANHO'), ('state', 'SITUAÇÃO')]:
            self.tree.heading(col, text=title)
        self.tree.column('#0', width=380, minwidth=220)
        self.tree.column('genre', width=190, minwidth=120)
        self.tree.column('tracks', width=80, anchor='center')
        self.tree.column('size', width=110, anchor='e')
        self.tree.column('state', width=170)
        scroll = ttk.Scrollbar(table, orient='vertical', command=self.tree.yview)
        self.tree.configure(yscrollcommand=scroll.set)
        self.tree.pack(side='left', fill='both', expand=True)
        scroll.pack(side='right', fill='y')
        self.tree.tag_configure('pending', foreground='#a15b00')
        self.tree.tag_configure('ready', foreground='#186b50')
        self.tree.tag_configure('blocked', foreground='#a52727')
        self.tree.bind('<<TreeviewSelect>>', self._selection_changed)
        self.tree.bind('<Double-1>', self._focus_genre)

        edit = ttk.Frame(root)
        edit.pack(fill='x', pady=(12, 8))
        ttk.Label(edit, text='Gênero dos álbuns selecionados:').pack(side='left')
        self.genre_var = tk.StringVar()
        self.genre_box = ttk.Combobox(edit, textvariable=self.genre_var, width=24, state='readonly')
        self.genre_box.pack(side='left', padx=(10, 8))
        self.genre_box.bind('<Return>', lambda _event: self._apply_genre())
        ttk.Button(edit, text='Aplicar gênero', command=self._apply_genre).pack(side='left')

        footer = ttk.Frame(root)
        footer.pack(fill='x', pady=(8, 0))
        self.summary = ttk.Label(footer, text='Nenhum álbum selecionado.')
        self.summary.pack(side='left')
        self.upload_btn = ttk.Button(footer, text='Revisar e enviar ao R2', command=self._start_prepare,
                                     style='Primary.TButton')
        self.upload_btn.pack(side='right')
        self.progress = ttk.Progressbar(root, mode='indeterminate')
        self.progress.pack(fill='x', pady=(12, 5))
        self.status_var = tk.StringVar(value='Pronto para adicionar pastas.')
        ttk.Label(root, textvariable=self.status_var).pack(anchor='w')

    def _work(self, target):
        if self.busy:
            return
        self.busy = True
        self.add_btn.state(['disabled'])
        self.upload_btn.state(['disabled'])
        self.progress.start(12)
        def run():
            try:
                target()
            except Exception as error:
                self.events.put(('error', str(error)))
            finally:
                self.events.put(('idle', None))
        threading.Thread(target=run, daemon=True).start()

    def _poll(self):
        while True:
            try:
                event, payload = self.events.get_nowait()
            except queue.Empty:
                break
            if event == 'scan':
                for album in payload:
                    key = str(album.path.resolve())
                    if key in self.albums and self.albums[key].genre:
                        album.genre = self.albums[key].genre
                        if not album.blocked:
                            album.note = ''
                    self.albums[key] = album
                    self.published.discard(key)
                    self.unchanged.discard(key)
                save_drafts(self.albums)
                self._render()
                self.status_var.set(f'{len(payload)} álbum(ns) encontrado(s).')
            elif event == 'catalog':
                index, genres = payload
                self.remote_index = index
                count = len(index['albums'])
                added = False
                for genre in sorted(genres):
                    try:
                        self.policy.resolve(genre)
                    except engine.GenreReviewNeeded:
                        try:
                            self.policy.add_genre(genre)
                            added = True
                        except ValueError:
                            self.status_var.set(f'Gênero remoto para revisar: {genre}')
                if added:
                    save_policy(self.policy)
                self._update_genre_values()
                self.server_label.configure(text=f'R2 · {count} álbum(ns) publicado(s)')
            elif event == 'prepared':
                self.after_idle(lambda value=payload: self._review(value))
            elif event == 'remote-prepared':
                self.after_idle(lambda value=payload: self._review_remote(value))
            elif event == 'remote-uploaded':
                self.status_var.set('Edição do acervo publicada no R2.')
                self.remote_index = None
                self._refresh_catalog()
                messagebox.showinfo('Edição publicada', 'Metadados atualizados. As jukeboxes receberão as alterações na próxima sincronização.')
            elif event == 'uploaded':
                changed = {(artist.casefold(), title.casefold()) for artist, title, _, _, _ in payload}
                for path, album in self.albums.items():
                    if (album.artist.casefold(), album.title.casefold()) in changed:
                        self.published.add(path)
                self._render()
                self.status_var.set('Publicação concluída. As jukeboxes poderão baixar o novo catálogo.')
                self.after_idle(self._refresh_catalog)
                messagebox.showinfo('Publicação concluída', 'Álbuns enviados e índice atualizado no R2.')
            elif event == 'status':
                self.status_var.set(payload)
            elif event == 'error':
                self.status_var.set('Falha: ' + payload)
                messagebox.showerror('Não foi possível concluir', payload)
            elif event == 'idle':
                self.busy = False
                self.add_btn.state(['!disabled'])
                self.upload_btn.state(['!disabled'])
                self.progress.stop()
        self.after(100, self._poll)

    def _add_folder(self):
        folder = filedialog.askdirectory(title='Escolha uma pasta de álbum, artista ou coleção')
        if not folder:
            return
        self.status_var.set('Analisando pastas e tags…')
        self._work(lambda: self.events.put(('scan', scan_folder(folder, self.policy))))

    def _refresh_catalog(self):
        def run():
            try:
                index = engine.existing_index(self.settings['base_url'])
                genres = set()
                for entry in index['albums']:
                    artist, title, genre = engine.existing_identity(entry)
                    entry.update(artist=artist, title=title, genre=genre)
                    genres.add(genre)
                self.events.put(('catalog', (index, genres)))
            except Exception as error:
                self.events.put(('status', f'Catálogo remoto indisponível: {error}'))
        threading.Thread(target=run, daemon=True).start()

    def _render(self):
        selection = {self.item_paths[item] for item in self.tree.selection() if item in self.item_paths}
        self.tree.delete(*self.tree.get_children())
        self.item_paths = {}
        parents = {}
        for path, album in sorted(self.albums.items(), key=lambda pair: (pair[1].artist.casefold(), pair[1].title.casefold())):
            if album.artist not in parents:
                parents[album.artist] = self.tree.insert('', 'end', text=album.artist, open=True)
            state = ('Verificar arquivos' if album.blocked else
                     'Definir gênero' if not album.genre else
                     'Publicado' if path in self.published else
                     'Já no servidor' if path in self.unchanged else 'Pronto')
            tag = 'blocked' if album.blocked else 'ready' if album.genre else 'pending'
            item = self.tree.insert(parents[album.artist], 'end', text=album.title,
                                    values=(album.genre or '—', album.tracks, f'{album.size / 1048576:.1f} MiB', state),
                                    tags=(tag,))
            self.item_paths[item] = path
            if path in selection:
                self.tree.selection_add(item)
        self._update_genre_values()
        self._summary()

    def _selected_albums(self):
        paths = set()
        for item in self.tree.selection():
            if self.tree.parent(item):
                paths.add(self.item_paths[item])
            else:
                for child in self.tree.get_children(item):
                    paths.add(self.item_paths[child])
        return [self.albums[path] for path in paths if path in self.albums]

    def _remove_selected(self):
        if self.busy:
            return
        for album in self._selected_albums():
            key = str(album.path.resolve())
            self.albums.pop(key, None)
            self.published.discard(key)
            self.unchanged.discard(key)
        save_drafts(self.albums)
        self._render()

    def _selection_changed(self, _event):
        selected = self._selected_albums()
        if len(selected) == 1:
            self.genre_var.set(selected[0].genre)
            if selected[0].note:
                self.status_var.set(selected[0].note)
        self._summary()

    def _focus_genre(self, _event):
        self.genre_box.focus_set()

    def _apply_genre(self):
        if self.busy:
            return
        try:
            genre = self.policy.canonical(self.genre_var.get().strip())
        except ValueError as error:
            messagebox.showerror('Gênero inválido', str(error))
            return
        selected = self._selected_albums()
        if not selected:
            messagebox.showinfo('Seleção', 'Selecione um álbum ou artista na lista.')
            return
        for album in selected:
            album.genre = genre
            key = str(album.path.resolve())
            self.published.discard(key)
            self.unchanged.discard(key)
            if not album.blocked:
                album.note = ''
        save_drafts(self.albums)
        self._render()

    def _update_genre_values(self):
        self.genre_box['values'] = sorted(self.policy.genres, key=str.casefold)

    def _genres_dialog(self):
        if self.busy:
            return
        window = tk.Toplevel(self)
        window.title('Gerenciar gêneros')
        window.transient(self)
        window.grab_set()
        window.resizable(False, False)
        frame = ttk.Frame(window, padding=18)
        frame.pack(fill='both')
        ttk.Label(frame, text='Gêneros disponíveis:').grid(row=0, column=0, columnspan=3, sticky='w')
        listed = tk.StringVar()
        ttk.Label(frame, textvariable=listed, wraplength=500).grid(row=1, column=0, columnspan=3, sticky='w', pady=(4, 16))
        ttk.Label(frame, text='Novo gênero:').grid(row=2, column=0, sticky='w')
        new_name = tk.StringVar()
        ttk.Entry(frame, textvariable=new_name, width=30).grid(row=2, column=1, padx=8, pady=6)
        ttk.Label(frame, text='Sinônimo na tag:').grid(row=3, column=0, sticky='w')
        alias_name = tk.StringVar()
        ttk.Entry(frame, textvariable=alias_name, width=30).grid(row=3, column=1, padx=8, pady=6)
        ttk.Label(frame, text='Usar gênero:').grid(row=4, column=0, sticky='w')
        target = tk.StringVar()
        target_box = ttk.Combobox(frame, textvariable=target, width=28, state='readonly')
        target_box.grid(row=4, column=1, padx=8, pady=6)
        ttk.Label(frame, text='Exemplo: Música Religiosa → Gospel. Tags com estilos distintos exigem revisão.',
                  wraplength=520).grid(row=5, column=0, columnspan=3, sticky='w', pady=(12, 6))

        def refresh():
            names = sorted(self.policy.genres, key=str.casefold)
            listed.set(' · '.join(names))
            target_box['values'] = names
            if not target.get() and names:
                target.set(names[0])
            self._update_genre_values()

        def update(kind):
            try:
                policy = engine.GenrePolicy(self.policy.genres, self.policy.aliases)
                if kind == 'genre':
                    name = policy.add_genre(new_name.get().strip())
                else:
                    policy.add_alias(alias_name.get().strip(), target.get())
                save_policy(policy)
            except ValueError as error:
                messagebox.showerror('Cadastro inválido', str(error), parent=window)
                return
            self.policy = policy
            if kind == 'genre':
                self.genre_var.set(name)
                new_name.set('')
            else:
                alias_name.set('')
            refresh()

        ttk.Button(frame, text='Cadastrar gênero', command=lambda: update('genre')).grid(row=2, column=2, padx=8)
        ttk.Button(frame, text='Cadastrar sinônimo', command=lambda: update('alias')).grid(row=3, column=2, padx=8)
        ttk.Button(frame, text='Fechar', command=window.destroy).grid(row=6, column=2, sticky='e', pady=(16, 0))
        refresh()

    def _summary(self):
        count = len(self.albums)
        pending = sum(not album.genre or album.blocked for album in self.albums.values())
        size = sum(album.size for album in self.albums.values()) / 1048576
        self.summary.configure(text=f'{count} álbum(ns) · {pending} pendente(s) · {size:.0f} MiB de áudio')

    def _start_prepare(self):
        drafts = list(self.albums.values())
        if not drafts:
            messagebox.showinfo('Adicionar álbuns', 'Escolha uma pasta para começar.')
            return
        blocked = [f'{a.artist} / {a.title}: {a.note}' for a in drafts if a.blocked]
        if blocked:
            messagebox.showwarning('Revisar arquivos', '\n'.join(blocked[:12]))
            return
        pending = [f'{a.artist} / {a.title}' for a in drafts if not a.genre]
        if pending:
            messagebox.showwarning('Definir gênero', 'Revise os gêneros pendentes:\n' + '\n'.join(pending[:12]))
            return
        settings = self.settings.copy()
        jobs = [(a.path, a.artist, a.genre) for a in drafts]
        def run():
            stage = Path(tempfile.mkdtemp(prefix='jukebox-office-'))
            try:
                self.events.put(('status', 'Conferindo catálogo e preparando álbuns…'))
                changes = engine.prepare_jobs(jobs, settings['base_url'], stage)
                self.events.put(('prepared', (stage, changes, settings)))
            except Exception:
                shutil.rmtree(stage, ignore_errors=True)
                raise
        self._work(run)

    def _review(self, payload):
        stage, changes, settings = payload
        if not changes:
            self.unchanged.update(self.albums)
            self._render()
            shutil.rmtree(stage, ignore_errors=True)
            self.status_var.set('Todos os álbuns selecionados já estão atualizados no R2.')
            messagebox.showinfo('Sem alterações', 'Nenhum arquivo novo para enviar.')
            return
        lines = [f'{genre} / {artist} / {title}' for artist, title, genre, _, _ in changes]
        changed = {(artist.casefold(), title.casefold()) for artist, title, _, _, _ in changes}
        self.unchanged.update(path for path, album in self.albums.items()
                              if (album.artist.casefold(), album.title.casefold()) not in changed)
        self._render()
        description = '\n'.join(lines[:15])
        if len(lines) > 15:
            description += f'\n… e mais {len(lines) - 15} álbum(ns)'
        if not messagebox.askyesno('Revisar publicação',
                                   f'{len(changes)} álbum(ns) serão publicados:\n\n{description}\n\nEnviar ao R2 agora?'):
            shutil.rmtree(stage, ignore_errors=True)
            self.status_var.set('Publicação cancelada. Nenhuma alteração foi feita no R2.')
            return
        def run():
            try:
                self.events.put(('status', 'Enviando faixas e capas…'))
                total = sum(1 for path in (stage / 'albums').rglob('*') if path.is_file())
                uploaded = 0
                def progress(line):
                    nonlocal uploaded
                    if line.startswith('upload:'):
                        uploaded += 1
                        name = line.split(' to s3://', 1)[0].rsplit('/', 1)[-1]
                        self.events.put(('status', f'Enviando {min(uploaded, total)}/{total}: {name}'))
                    elif line.strip():
                        self.events.put(('status', line))
                engine.publish(stage, settings['bucket'], settings['profile'],
                               settings['endpoint'], settings['base_url'],
                               progress=progress)
                self.events.put(('uploaded', changes))
            finally:
                shutil.rmtree(stage, ignore_errors=True)
        self._work(run)

    def _remote_dialog(self):
        if self.busy:
            return
        if self.remote_index is None:
            self._refresh_catalog()
            messagebox.showinfo('Carregando acervo', 'Aguarde a leitura do catálogo e abra Acervo do servidor novamente.')
            return
        window = tk.Toplevel(self)
        window.title('Acervo do servidor · Editar álbuns')
        window.geometry('940x600')
        window.minsize(760, 500)
        window.transient(self)
        window.grab_set()
        frame = ttk.Frame(window, padding=16)
        frame.pack(fill='both', expand=True)
        ttk.Label(frame, text='Selecione um álbum para editar. Para mudar o gênero de vários, selecione-os juntos.',
                  wraplength=700).pack(anchor='w', pady=(0, 10))
        table = ttk.Frame(frame)
        table.pack(fill='both', expand=True)
        tree = ttk.Treeview(table, columns=('artist', 'title', 'genre', 'state'), show='headings', selectmode='extended')
        for name, label in [('artist', 'ARTISTA'), ('title', 'ÁLBUM'), ('genre', 'GÊNERO'), ('state', 'SITUAÇÃO')]:
            tree.heading(name, text=label)
            tree.column(name, width=200 if name in ('artist', 'title') else 140, minwidth=80)
        scroll = ttk.Scrollbar(table, orient='vertical', command=tree.yview)
        tree.configure(yscrollcommand=scroll.set)
        tree.pack(side='left', fill='both', expand=True)
        scroll.pack(side='right', fill='y')
        entries = {entry['id']: dict(entry) for entry in self.remote_index['albums']}
        expected_version = self.remote_index['version']
        edits = {}
        for aid, entry in sorted(entries.items(), key=lambda pair: (engine.genre_key(pair[1]['artist']), engine.genre_key(pair[1]['title']))):
            tree.insert('', 'end', iid=aid, values=(entry['artist'], entry['title'], entry['genre'], 'Publicado'))
        fields = ttk.Frame(frame)
        fields.pack(fill='x', pady=12)
        artist, title, genre = tk.StringVar(), tk.StringVar(), tk.StringVar()
        artist_entry = ttk.Entry(fields, textvariable=artist)
        title_entry = ttk.Entry(fields, textvariable=title)
        genre_box = ttk.Combobox(fields, textvariable=genre, values=sorted(self.policy.genres, key=str.casefold), state='readonly')
        for col, (label, widget) in enumerate([('Artista', artist_entry), ('Álbum', title_entry), ('Gênero', genre_box)]):
            fields.columnconfigure(col, weight=1)
            ttk.Label(fields, text=label).grid(row=0, column=col, sticky='w', padx=4)
            widget.grid(row=1, column=col, sticky='ew', padx=4)
        status = tk.StringVar(value='Nenhuma alteração pendente.')
        ttk.Label(frame, textvariable=status).pack(anchor='w')

        def select(_event):
            selected = tree.selection()
            if not selected:
                return
            record = edits.get(selected[0], entries[selected[0]])
            artist.set(record['artist'] if len(selected) == 1 else '')
            title.set(record['title'] if len(selected) == 1 else '')
            for widget in (artist_entry, title_entry):
                widget.configure(state='normal' if len(selected) == 1 else 'disabled')
            try:
                genre.set(self.policy.resolve(record['genre']))
            except ValueError:
                genre.set('')

        def apply():
            selected = tree.selection()
            if not selected:
                return
            try:
                canonical = self.policy.canonical(genre.get())
                if len(selected) == 1:
                    new_artist = engine.valid_component(artist.get().strip())
                    new_title = engine.valid_component(title.get().strip())
            except ValueError as error:
                messagebox.showerror('Revisar edição', str(error), parent=window)
                return
            for aid in selected:
                record = edits.get(aid, entries[aid])
                updated = dict(artist=new_artist if len(selected) == 1 else record['artist'],
                               title=new_title if len(selected) == 1 else record['title'], genre=canonical)
                if all(updated[key] == entries[aid][key] for key in updated):
                    edits.pop(aid, None)
                else:
                    edits[aid] = updated
                tree.item(aid, values=(updated['artist'], updated['title'], updated['genre'], 'Editado' if aid in edits else 'Publicado'))
            status.set(f'{len(edits)} álbum(ns) com alterações para publicar.')

        def prepare():
            if self.busy:
                return
            if not edits:
                messagebox.showinfo('Editar álbuns', 'Clique em Aplicar edição antes de publicar.', parent=window)
                return
            settings = self.settings.copy()
            pending = {aid: dict(fields) for aid, fields in edits.items()}
            policy = engine.GenrePolicy(self.policy.genres, self.policy.aliases)
            window.destroy()
            def run():
                stage = Path(tempfile.mkdtemp(prefix='jukebox-edit-'))
                try:
                    self.events.put(('status', 'Preparando edição do acervo remoto…'))
                    changes = engine.prepare_metadata_edits(pending, settings['base_url'], stage, policy,
                                                            expected_version=expected_version)
                    self.events.put(('remote-prepared', (stage, changes, settings)))
                except Exception:
                    shutil.rmtree(stage, ignore_errors=True)
                    raise
            self._work(run)

        tree.bind('<<TreeviewSelect>>', select)
        buttons = ttk.Frame(frame)
        buttons.pack(fill='x', pady=(12, 0))
        ttk.Button(buttons, text='Aplicar edição', command=apply).pack(side='left')
        ttk.Button(buttons, text='Fechar', command=window.destroy).pack(side='right')
        ttk.Button(buttons, text='Revisar e publicar edições', command=prepare, style='Primary.TButton').pack(side='right', padx=8)

    def _review_remote(self, payload):
        stage, changes, settings = payload
        if not changes:
            shutil.rmtree(stage, ignore_errors=True)
            self.status_var.set('Nenhuma mudança no acervo remoto.')
            return
        description = '\n'.join(f'{genre} / {artist} / {title}' for artist, title, genre, _, _ in changes[:15])
        if len(changes) > 15:
            description += f'\n… e mais {len(changes) - 15} álbum(ns)'
        if not messagebox.askyesno('Publicar edição do acervo',
                                   f'{len(changes)} álbum(ns) serão atualizados:\n\n{description}\n\nAs músicas serão reaproveitadas. Publicar agora?'):
            shutil.rmtree(stage, ignore_errors=True)
            self.status_var.set('Edição cancelada.')
            return
        def run():
            try:
                engine.publish(stage, settings['bucket'], settings['profile'], settings['endpoint'],
                               settings['base_url'], progress=lambda line: self.events.put(('status', line)))
                self.events.put(('remote-uploaded', None))
            finally:
                shutil.rmtree(stage, ignore_errors=True)
        self._work(run)

    def _settings_dialog(self):
        if self.busy:
            return
        window = tk.Toplevel(self)
        window.title('Servidor R2')
        window.transient(self)
        window.grab_set()
        fields = [('base_url', 'URL pública HTTPS'), ('bucket', 'Bucket'),
                  ('profile', 'Perfil AWS CLI'), ('endpoint', 'Endpoint S3')]
        vars_ = {}
        for row, (key, label) in enumerate(fields):
            ttk.Label(window, text=label).grid(row=row, column=0, sticky='w', padx=16, pady=10)
            value = tk.StringVar(value=self.settings[key])
            ttk.Entry(window, textvariable=value, width=68).grid(row=row, column=1, padx=16, pady=10)
            vars_[key] = value
        def save():
            try:
                settings = {key: value.get().strip() for key, value in vars_.items()}
                save_settings(settings)
            except Exception as error:
                messagebox.showerror('Configuração inválida', str(error), parent=window)
                return
            self.settings = settings
            self.remote_index = None
            window.destroy()
            self._refresh_catalog()
        ttk.Button(window, text='Salvar', command=save, style='Primary.TButton').grid(row=len(fields), column=1, sticky='e', padx=16, pady=16)

    def _close(self):
        if self.busy:
            messagebox.showinfo('Operação em andamento', 'Aguarde terminar a leitura ou publicação do lote.')
            return
        self.destroy()


if __name__ == '__main__':
    OfficePublisher().mainloop()
