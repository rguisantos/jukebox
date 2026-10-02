//! Janela X11 filha da janela Slint — o VLC desenha o vídeo nela.
//!
//! É a mesma técnica do programa Java original (vlcj embutia o VLC num
//! `Canvas` AWT): o `libvlc_video_set_xwindow` recebe o XID de uma janela
//! filha e o VLC 1.1 da base renderiza direto nela, com Xv/X11 — **zero
//! cópia de pixels pela CPU** (fundamental num Pentium 4 software-only).
//!
//! A janela é criada por uma conexão Xlib própria (`XOpenDisplay`), o que
//! é legal em X11: qualquer cliente pode criar janela filha de qualquer
//! janela (o gerenciador de janelas só reserva `SubstructureRedirectMask`
//! na raiz). A geometria chega do event loop do Slint a cada tick (ver
//! `legacy_vlc::overlay_sync`), então a janela acompanha o layout e o
//! modo tela cheia sem lógica no player.
//!
//! `libX11.so.6` é carregada por soname via `libloading` — o binário não
//! linka X11 em tempo de build e o mesmo binário roda sem X (áudio só).

use libloading::Library;
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};

/// Soname padrão (a base antiga é X11 puro, sem Wayland).
const LIBX11_SONAME: &str = "libX11.so.6";

#[allow(non_snake_case)]
mod x11 {
    use super::*;

    pub type OpenDisplayFn = unsafe extern "C" fn(name: *const c_char) -> *mut c_void;
    pub type CloseDisplayFn = unsafe extern "C" fn(display: *mut c_void) -> c_int;
    pub type CreateSimpleWindowFn = unsafe extern "C" fn(
        display: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    pub type MapWindowFn = unsafe extern "C" fn(display: *mut c_void, window: c_ulong) -> c_int;
    pub type UnmapWindowFn = unsafe extern "C" fn(display: *mut c_void, window: c_ulong) -> c_int;
    pub type MoveResizeWindowFn = unsafe extern "C" fn(
        display: *mut c_void,
        window: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
    ) -> c_int;
    pub type DestroyWindowFn = unsafe extern "C" fn(display: *mut c_void, window: c_ulong) -> c_int;
    pub type FlushFn = unsafe extern "C" fn(display: *mut c_void) -> c_int;
}

/// Símbolos Xlib usados pelo overlay (dlopen por soname).
struct X11Lib {
    open_display: x11::OpenDisplayFn,
    close_display: x11::CloseDisplayFn,
    create_simple_window: x11::CreateSimpleWindowFn,
    map_window: x11::MapWindowFn,
    unmap_window: x11::UnmapWindowFn,
    move_resize_window: x11::MoveResizeWindowFn,
    destroy_window: x11::DestroyWindowFn,
    flush: x11::FlushFn,
    /// Por último: fecha a biblioteca depois dos ponteiros saírem de uso.
    _lib: Library,
}

impl X11Lib {
    fn open() -> Result<Self, String> {
        let soname = std::env::var("JUKEBOX_LIBX11")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| LIBX11_SONAME.into());
        unsafe {
            let lib = Library::new(&soname).map_err(|e| format!("dlopen {soname}: {e}"))?;
            // Cada ponteiro é carregado com o tipo já correto — o tipo é
            // responsabilidade nossa e espelha o protótipo do Xlib.
            let open_display = *lib
                .get::<x11::OpenDisplayFn>(b"XOpenDisplay\0")
                .map_err(|e| format!("dlsym XOpenDisplay: {e}"))?;
            let close_display = *lib
                .get::<x11::CloseDisplayFn>(b"XCloseDisplay\0")
                .map_err(|e| format!("dlsym XCloseDisplay: {e}"))?;
            let create_simple_window = *lib
                .get::<x11::CreateSimpleWindowFn>(b"XCreateSimpleWindow\0")
                .map_err(|e| format!("dlsym XCreateSimpleWindow: {e}"))?;
            let map_window = *lib
                .get::<x11::MapWindowFn>(b"XMapWindow\0")
                .map_err(|e| format!("dlsym XMapWindow: {e}"))?;
            let unmap_window = *lib
                .get::<x11::UnmapWindowFn>(b"XUnmapWindow\0")
                .map_err(|e| format!("dlsym XUnmapWindow: {e}"))?;
            let move_resize_window = *lib
                .get::<x11::MoveResizeWindowFn>(b"XMoveResizeWindow\0")
                .map_err(|e| format!("dlsym XMoveResizeWindow: {e}"))?;
            let destroy_window = *lib
                .get::<x11::DestroyWindowFn>(b"XDestroyWindow\0")
                .map_err(|e| format!("dlsym XDestroyWindow: {e}"))?;
            let flush = *lib
                .get::<x11::FlushFn>(b"XFlush\0")
                .map_err(|e| format!("dlsym XFlush: {e}"))?;
            Ok(Self {
                open_display,
                close_display,
                create_simple_window,
                map_window,
                unmap_window,
                move_resize_window,
                destroy_window,
                flush,
                _lib: lib,
            })
        }
    }
}

/// Janela filha dedicada ao vídeo do VLC.
///
/// Uma conexão Xlib dedicada: após a criação (thread do player), a janela
/// só é tocada pelo event loop do Slint (`place`), uma thread por vez — o
/// padrão seguro para `Display*` sem `XInitThreads`.
pub struct VideoOverlay {
    x11: X11Lib,
    display: *mut c_void,
    window: c_ulong,
    mapped: bool,
    last: (i32, i32, c_uint, c_uint),
}

// A conexão atravessa a thread do player → event loop do Slint sempre
// uma thread por vez (criação numa, uso na outra), sem acesso concorrente.
unsafe impl Send for VideoOverlay {}

impl VideoOverlay {
    /// Cria a janela filha (1×1, preta, desmapeada) de `parent_xid`.
    pub fn new(parent_xid: u64) -> Result<Self, String> {
        if parent_xid == 0 {
            return Err("XID da janela pai ausente".into());
        }
        let x11 = X11Lib::open()?;
        unsafe {
            let display = (x11.open_display)(std::ptr::null());
            if display.is_null() {
                return Err("XOpenDisplay falhou (DISPLAY acessível?)".into());
            }
            // Pixel de fundo 0 = preto nos visores TrueColor da base;
            // sem borda; coordenadas relativas ao interior da janela pai.
            let window = (x11.create_simple_window)(
                display,
                parent_xid as c_ulong,
                0,
                0,
                1,
                1,
                0,
                0,
                0,
            );
            if window == 0 {
                (x11.close_display)(display);
                return Err("XCreateSimpleWindow falhou".into());
            }
            Ok(Self { x11, display, window, mapped: false, last: (0, 0, 0, 0) })
        }
    }

    /// XID entregue ao `libvlc_video_set_xwindow`.
    pub fn window_id(&self) -> u32 {
        self.window as u32
    }

    /// Posiciona/redimensiona e (des)mapeia. Chamado pelo event loop a
    /// cada tick — só toca o servidor quando algo mudou de fato.
    pub fn place(&mut self, x: i32, y: i32, w: u32, h: u32, visible: bool) {
        if w == 0 || h == 0 {
            return; // layout ainda não medido
        }
        unsafe {
            if !visible {
                if self.mapped {
                    (self.x11.unmap_window)(self.display, self.window);
                    self.mapped = false;
                    (self.x11.flush)(self.display);
                }
                return;
            }
            if self.last != (x, y, w, h) {
                (self.x11.move_resize_window)(
                    self.display,
                    self.window,
                    x,
                    y,
                    w as c_uint,
                    h as c_uint,
                );
                self.last = (x, y, w, h);
            }
            if !self.mapped {
                (self.x11.map_window)(self.display, self.window);
                self.mapped = true;
            }
            (self.x11.flush)(self.display);
        }
    }
}

impl Drop for VideoOverlay {
    fn drop(&mut self) {
        unsafe {
            if self.window != 0 {
                (self.x11.destroy_window)(self.display, self.window);
            }
            (self.x11.close_display)(self.display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_requires_a_parent_xid() {
        // Sem XID não há janela filha: o erro é explícito (o app segue
        // com áudio; o vídeo ficaria sem janela embutida).
        assert!(VideoOverlay::new(0).is_err());
    }
}
