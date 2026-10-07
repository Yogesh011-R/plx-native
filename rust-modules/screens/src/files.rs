//! **The file browser** — TVPlayer's root page. It lists the folders and video files on a USB
//! drive (or any media root), opens a folder on OK, goes up on BACK, and asks the application to
//! play a video file ([`AppFx::PlayFile`]).
//!
//! **A video is read before it is played.** OK on a file probes it on a worker
//! (`player::local::probe`: its container, codecs, frame rate and Dolby Vision) and shows
//! "Opening…" meanwhile. A file the television can play becomes [`AppFx::PlayFile`] carrying the
//! Load declaration; one it cannot keeps the viewer here, with the reason as a note at the top of
//! the list. The note goes with the next folder change or the next play.
//!
//! **Where it looks.** `PLXNATIVE_MEDIA_ROOT` when set (the simulator's dev knob), else the first
//! of [`TV_ROOTS`] that exists on the television, else `$HOME/Videos`, else `$HOME`. The root is
//! resolved once, at mount.
//!
//! **The listing is off the frame.** A USB stick can take seconds to answer its first `readdir`,
//! so a folder is read on a worker (`task::spawn_off_frame`) and lands on a later `Tick`, waking
//! the present gate the same way the subtitle workers do. A listing for a folder the viewer has
//! already left is dropped by its generation.
//!
//! **Rows are keyed by position.** A folder's rows are a dynamic list with no other identity, which
//! is the documented key for one (see `CLAUDE.md`, the FormTable paragraph). Every listing is a
//! fresh `open`, so a key never carries over from one folder to the next as the same row.
//!
//! **BACK at the media root is not handled here**: the dispatcher hands it to the television, as
//! it does for every root page.

use std::borrow::Cow;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::registry::{AppFx, AppLike};
use plx_media::player::local::{LocalPlay, Refusal};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine,
};
use plx_ui::form::{Activation, Form, FormSection, FormTable, RowKey, RowKeys, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteGround;
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use plx_ui::table::Row;
use plx_ui::{Painter, Rect};

pub const SHAPE: &str =
    "Files{root:str,cwd:str,generation:u32,state:{Loading,Ready([{name:str,dir:bool}]),Missing,Failed},trail:[u32],notice:{None,Opening,Refused(str)},sel:u32,table:TableViewMotion}";

/// Where webOS mounts USB storage, in the order tried. Unverified on a set: the first one that
/// exists wins, and `PLXNATIVE_MEDIA_ROOT` overrides all of them.
const TV_ROOTS: &[&str] = &["/tmp/usb", "/media/usb", "/mnt/usb"];

/// File extensions the browser offers as videos (lower-case, without the dot). What actually
/// plays is the player's decision; this only decides what is listed.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "ts", "m2ts", "mts", "webm", "mpg", "mpeg", "wmv", "flv",
    "3gp",
];

/// One listed folder or video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// The line above the rows about the last video chosen.
#[derive(Clone, PartialEq, Eq)]
enum Notice {
    None,
    /// The chosen file is being read.
    Opening,
    /// The chosen file cannot be played here, and why.
    Refused(Refusal),
}

type ProbeResult = Result<LocalPlay, Refusal>;

/// A codec as the viewer reads it: FFmpeg's name, upper-cased (`vp9` → `VP9`).
fn codec_label(codec: &str) -> String {
    codec.to_ascii_uppercase()
}

/// The viewer's wording for a refusal.
fn refusal_text(why: &Refusal) -> String {
    use plx_platform::i18n::msg;
    match why {
        Refusal::PlayerUnavailable => msg::files_cant_play_player().to_string(),
        Refusal::Unreadable => msg::files_cant_play_unreadable().to_string(),
        Refusal::NoVideo => msg::files_cant_play_no_video().to_string(),
        Refusal::VideoCodec(c) => msg::files_cant_play_video_codec(&codec_label(c)).to_string(),
        Refusal::NoAudio => msg::files_cant_play_no_audio().to_string(),
        Refusal::AudioCodec(c) => msg::files_cant_play_audio_codec(&codec_label(c)).to_string(),
        Refusal::DolbyVision(_) => msg::files_cant_play_dolby_vision().to_string(),
    }
}

/// What the current folder looks like right now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Listing {
    Loading,
    Ready(Vec<Entry>),
    /// The media root does not exist (no drive plugged in).
    Missing,
    /// The folder exists but could not be read.
    Failed,
}

/// The media root, resolved from the environment and the filesystem.
pub fn media_root() -> PathBuf {
    if let Some(root) = std::env::var_os("PLXNATIVE_MEDIA_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    if let Some(root) = TV_ROOTS.iter().map(Path::new).find(|p| p.is_dir()) {
        return root.to_path_buf();
    }
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if home.join("Videos").is_dir() => home.join("Videos"),
        Some(home) => home,
        None => PathBuf::from("/"),
    }
}

/// Is `name` a file the browser lists as a video?
pub fn is_video(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|v| e.eq_ignore_ascii_case(v)))
}

/// Read one folder: visible subfolders first, then videos, each group sorted case-insensitively.
/// Pure over the filesystem, so a test can drive it with a temporary directory.
fn read_listing(dir: &Path) -> Listing {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Listing::Missing,
        Err(_) => return Listing::Failed,
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            // `metadata` follows symlinks, so a linked folder lists as a folder.
            let is_dir = std::fs::metadata(e.path()).ok()?.is_dir();
            (is_dir || is_video(&name)).then_some(Entry { name, dir: is_dir })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Listing::Ready(entries)
}

/// A row's focus key: its position plus one, so key 0 is never a row.
fn row_key(index: usize) -> RowKey {
    RowKey(index as u32 + 1)
}

/// The table's frame: the page between the top margin and the bottom overscan line.
fn frame() -> Rect {
    use plx_ui::consts::{MARGIN_X, SCR_H, SCR_W};
    let top = 120.0;
    Rect::new(MARGIN_X, top, SCR_W - 2.0 * MARGIN_X, SCR_H - top - 80.0)
}

pub struct FilesScreen {
    entry: EntryId,
    root: PathBuf,
    cwd: PathBuf,
    listing: Listing,
    /// Bumped on every folder change; a landing for an older generation is dropped.
    generation: u32,
    pending: Option<(u32, Receiver<Listing>)>,
    /// The row index chosen in each folder above `cwd`, so BACK lands on the folder it left.
    trail: Vec<usize>,
    /// The row to select when the next listing lands (set by BACK).
    restore: Option<usize>,
    notice: Notice,
    /// The probe of the video being opened. At most one runs; OK on another video meanwhile is
    /// ignored.
    opening: Option<Receiver<ProbeResult>>,
    form: FormTable<usize, usize, Infallible>,
    ground: RouteGround,
}

impl FilesScreen {
    pub fn new(entry: EntryId) -> Self {
        let root = media_root();
        Self {
            entry,
            cwd: root.clone(),
            root,
            listing: Listing::Loading,
            generation: 0,
            pending: None,
            trail: Vec::new(),
            restore: None,
            notice: Notice::None,
            opening: None,
            form: FormTable::new(crate::registry::BAND),
            ground: RouteGround::new(),
        }
    }

    /// Start reading `cwd` on a worker. The table shows the loading note until it lands.
    fn load(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.listing = Listing::Loading;
        if self.opening.is_none() {
            self.notice = Notice::None;
        }
        self.rebuild();
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = self.cwd.clone();
        let spawned = plx_base::task::spawn_off_frame("files-list", move |_| {
            let _ = tx.send(read_listing(&dir));
            plx_machine::present::wake_from_worker();
        });
        if spawned {
            self.pending = Some((self.generation, rx));
        } else {
            self.listing = Listing::Failed;
            self.rebuild();
        }
    }

    /// Read the chosen video on a worker; [`Self::poll_probe`] takes the answer.
    fn open_video(&mut self, path: PathBuf) {
        if self.opening.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = plx_base::task::spawn_off_frame("files-probe", move |_| {
            let _ = tx.send(plx_media::player::local::probe(&path));
            plx_machine::present::wake_from_worker();
        });
        if spawned {
            self.opening = Some(rx);
            self.set_notice(Notice::Opening);
        } else {
            self.set_notice(Notice::Refused(Refusal::Unreadable));
        }
    }

    /// Take a landed probe: a playable file is handed to the application, a refusal is shown.
    fn poll_probe<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) -> bool {
        let Some(rx) = &self.opening else { return false };
        let landed = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(Refusal::Unreadable),
        };
        self.opening = None;
        match landed {
            Ok(play) => {
                self.set_notice(Notice::None);
                fx.push(Fx::App(AppFx::PlayFile(play)));
            }
            Err(why) => {
                plx_base::eventlog::log(&format!("files: cannot play ({})", why.log_form()));
                self.set_notice(Notice::Refused(why));
            }
        }
        true
    }

    /// Change the notice, keeping the selected row.
    fn set_notice(&mut self, notice: Notice) {
        if self.notice == notice {
            return;
        }
        self.notice = notice;
        self.restore = self.form.selected_key().map(|k| k.0 as usize - 1);
        self.rebuild();
    }

    /// Take a landed listing, if the worker has answered for the current folder.
    fn poll(&mut self) -> bool {
        let Some((generation, rx)) = &self.pending else { return false };
        let landed = match rx.try_recv() {
            Ok(listing) => Some(listing),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Some(Listing::Failed),
        };
        let current = *generation == self.generation;
        self.pending = None;
        match landed {
            Some(listing) if current => {
                self.listing = listing;
                self.rebuild();
                true
            }
            _ => false,
        }
    }

    /// The heading: the media root's name at the top, else the path below the root.
    fn heading(&self) -> String {
        match self.cwd.strip_prefix(&self.root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => plx_platform::i18n::msg::files_title().to_string(),
        }
    }

    fn rebuild(&mut self) {
        use plx_platform::i18n::msg;
        let mut section = FormSection::<usize, usize, Infallible>::new(self.heading());
        match &self.notice {
            Notice::None => {}
            Notice::Opening => section = section.note(msg::files_opening()),
            Notice::Refused(why) => section = section.note(refusal_text(why)),
        }
        match &self.listing {
            Listing::Loading => section = section.note(msg::files_loading()),
            Listing::Missing => section = section.note(msg::files_no_drive()),
            Listing::Failed => section = section.note(msg::files_unreadable()),
            Listing::Ready(entries) if entries.is_empty() => {
                section = section.note(if self.cwd == self.root {
                    msg::files_no_drive()
                } else {
                    msg::files_empty()
                })
            }
            Listing::Ready(entries) => {
                for (i, e) in entries.iter().enumerate() {
                    let row = Row::new(e.name.clone()).chevron(e.dir);
                    section = section.item_keyed(i, row_key(i), RowKind::Button, i, row);
                }
            }
        }
        let initial = self.restore.take();
        self.form.open(Form::new().section(section), initial.as_ref());
    }

    fn entries(&self) -> &[Entry] {
        match &self.listing {
            Listing::Ready(entries) => entries,
            _ => &[],
        }
    }

    /// OK on a row: enter a folder, or ask the application to play a video.
    fn activate(&mut self, elem: u32) {
        let Some(index) = self
            .form
            .index_of_key(RowKey(elem))
            .and_then(|i| self.form.activate(i))
            .and_then(|a| match a {
                Activation::Action(index) => Some(index),
                Activation::Push(never) => match never {},
            })
        else {
            return;
        };
        let Some(entry) = self.entries().get(index).cloned() else { return };
        let path = self.cwd.join(&entry.name);
        if entry.dir {
            self.trail.push(index);
            self.cwd = path;
            self.load();
        } else {
            self.open_video(path);
        }
    }

    /// BACK below the root: go up one folder and land on the folder just left.
    fn up(&mut self) -> bool {
        if self.cwd == self.root {
            return false;
        }
        let Some(parent) = self.cwd.parent().map(Path::to_path_buf) else { return false };
        self.cwd = parent;
        self.restore = self.trail.pop();
        self.load();
        true
    }
}

impl<H: AppLike> Machine<H> for FilesScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => self.load(),
            ScreenEvent::Tick(tick) => {
                if self.poll() | self.poll_probe(fx) {
                    fx.invalidate(plx_machine::present::Provenance::Landing(fx.from()));
                }
                self.form.table.update(tick.dt(), frame().h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.form.note_engine_key(Some(RowKey(to.elem)));
                if let Some(row) = self.form.index_of_key(RowKey(to.elem)) {
                    self.form.table.sel = row as i32;
                }
            }
            ScreenEvent::Activate(elem) => self.activate(*elem),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem);
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    if self.up() {
                        return Handled::Yes;
                    }
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: AppLike> Focusable<H> for FilesScreen {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: frame(),
            len: self.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.form.index_of_key(RowKey(*elem)).map(|_| GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.form.step_key(RowKey(key.elem), delta) {
            Some(next) => Step::Move(FocusKey { entry: self.entry, elem: next.0 }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, _: &Cx<'_, H>, _: At) -> Option<Placed> {
        let row = self.form.index_of_key(RowKey(*elem))?;
        let rect = self.form.table.row_frame(frame(), row as i32)?;
        Some(Placed { rect, rest_rect: rect, clip: frame(), index: Some(row as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        // A new listing's landing row wins over the engine's key (which names a row of the folder
        // just left); a key this folder has no row for settles on the table's own selection.
        let elem = self
            .form
            .reseat()
            .or_else(|| self.group_of(&want.elem, cx).map(|_| RowKey(want.elem)))
            .or_else(|| self.form.selected_key())
            .map_or(0, |k| k.0);
        FocusKey { entry: self.entry, elem }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: self.form.selected_key().map_or(0, |k| k.0) }
    }
}

impl<H: AppLike> Screen<H> for FilesScreen {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        crate::registry::word::FILES
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        // The ground on the root painter, as the sign-in page does: the ambient wash must not
        // ride the page-transition cascade.
        self.ground.draw_default(Painter::root());
        let p = f.painter;
        let r = frame();
        self.form.table.draw(p, r, f.measure);
        for elem in (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.key_at(i).map(|k| k.0)) {
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey { entry: self.entry, elem },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl LogicalState for FilesScreen {
    fn write(&self, c: &mut Canon) {
        c.str(&self.root.to_string_lossy()).str(&self.cwd.to_string_lossy()).u32(self.generation);
        match &self.listing {
            Listing::Loading => {
                c.u32(0);
            }
            Listing::Ready(entries) => {
                c.u32(1).seq(entries.len());
                for e in entries {
                    c.str(&e.name).u8(u8::from(e.dir));
                }
            }
            Listing::Missing => {
                c.u32(2);
            }
            Listing::Failed => {
                c.u32(3);
            }
        }
        c.seq(self.trail.len());
        for i in &self.trail {
            c.u32(*i as u32);
        }
        match &self.notice {
            Notice::None => {
                c.u32(0);
            }
            Notice::Opening => {
                c.u32(1);
            }
            Notice::Refused(why) => {
                c.u32(2).str(&why.log_form());
            }
        }
        c.u32(self.form.table.sel as u32);
        self.form.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("files");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tvplayer-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn video_extensions_match_case_insensitively() {
        assert!(is_video("Film.MKV"));
        assert!(is_video("clip.mp4"));
        assert!(!is_video("notes.txt"));
        assert!(!is_video("mkv"));
    }

    #[test]
    fn a_listing_puts_folders_first_and_hides_the_rest() {
        let dir = scratch("listing");
        std::fs::create_dir(dir.join("b-folder")).unwrap();
        std::fs::create_dir(dir.join("A-folder")).unwrap();
        std::fs::create_dir(dir.join(".hidden")).unwrap();
        for f in ["z.mkv", "a.MP4", "readme.txt", ".secret.mkv"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let names = match read_listing(&dir) {
            Listing::Ready(entries) => entries.into_iter().map(|e| (e.name, e.dir)).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            names,
            [
                ("A-folder".to_string(), true),
                ("b-folder".to_string(), true),
                ("a.MP4".to_string(), false),
                ("z.mkv".to_string(), false),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_root_is_reported_as_no_drive() {
        let dir = scratch("missing").join("not-there");
        assert_eq!(read_listing(&dir), Listing::Missing);
    }
}
