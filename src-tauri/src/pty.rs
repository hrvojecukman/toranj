//! PTY bridge: one `tmux attach` client per visible tab, streamed to xterm.js.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::ipc::{Channel, InvokeResponseBody};

struct Client {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Default)]
pub struct Bridge {
    next: AtomicU32,
    clients: Mutex<HashMap<u32, Client>>,
}

impl Bridge {
    pub fn attach(
        &self,
        session: &str,
        cols: u16,
        rows: u16,
        out: Channel<InvokeResponseBody>,
    ) -> Result<u32, String> {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;

        let mut cmd = CommandBuilder::new(crate::tmux::bin());
        // `=` makes the target exact: "x" must not attach "x-2".
        cmd.args(["-L", crate::tmux::SOCKET, "attach-session", "-t", &format!("={session}")]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env(
            "LANG",
            std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into()),
        );
        let child = pty.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pty.master.take_writer().map_err(|e| e.to_string())?;
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;

        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out
                            .send(InvokeResponseBody::Raw(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        self.clients.lock().unwrap().insert(
            id,
            Client {
                master: pty.master,
                writer,
                child,
            },
        );
        Ok(id)
    }

    pub fn write(&self, id: u32, data: &[u8]) -> Result<(), String> {
        let mut map = self.clients.lock().unwrap();
        let c = map.get_mut(&id).ok_or("unknown terminal")?;
        c.writer.write_all(data).map_err(|e| e.to_string())
    }

    pub fn resize(&self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        let map = self.clients.lock().unwrap();
        let c = map.get(&id).ok_or("unknown terminal")?;
        c.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }

    /// Kills only the attach client; the tmux session keeps running.
    pub fn detach(&self, id: u32) {
        if let Some(mut c) = self.clients.lock().unwrap().remove(&id) {
            let _ = c.child.kill();
            let _ = c.child.wait();
        }
    }

    pub fn detach_all(&self) {
        let ids: Vec<u32> = self.clients.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.detach(id);
        }
    }
}
