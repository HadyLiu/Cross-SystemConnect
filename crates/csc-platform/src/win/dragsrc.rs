//! OLE drag source for files that are still on another computer.
//!
//! The data object offers virtual files (`FileGroupDescriptorW` + `FileContents`).
//! Explorer reads each file through a stream over the staging copy, which the
//! core fills while the network delivers it; reads wait for bytes that are not
//! there yet. With async extraction Explorer copies in the background after the
//! drop, so the drag ends at once and Explorer shows its own copy progress.

use crate::vdrag::{DragDesc, DragEvent, DragItem, StagedFeed, Wait};
use std::io::{Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use windows::core::{implement, w, Error, Result, HRESULT};
use windows::Win32::Foundation::{
    BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC,
    E_NOTIMPL, E_UNEXPECTED, FILETIME, HGLOBAL, OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA,
    ISequentialStream_Impl, IStream, IStream_Impl, DATADIR_GET, DVASPECT_CONTENT, FORMATETC,
    LOCKTYPE, STATFLAG, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0, STREAM_SEEK, STREAM_SEEK_CUR,
    STREAM_SEEK_END, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, ReleaseStgMedium,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc,
    FILEDESCRIPTORW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

const FD_ATTRIBUTES: u32 = 0x0000_0004;
const FD_WRITESTIME: u32 = 0x0000_0020;
const FD_FILESIZE: u32 = 0x0000_0040;
const FD_PROGRESSUI: u32 = 0x0000_4000;
const FD_UNICODE: u32 = 0x8000_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const MK_LBUTTON: u32 = 0x0001;
const STGTY_STREAM: u32 = 2;
/// A read gives up when no byte of the file arrived for this long.
const STALL_LIMIT: Duration = Duration::from_secs(120);
/// Background copy by Explorer that never reports its end.
const EXTRACT_LIMIT: Duration = Duration::from_secs(6 * 3600);

static CANCEL: AtomicU64 = AtomicU64::new(0);

pub fn cancel(id: u64) {
    CANCEL.store(id, Ordering::Release);
}

pub fn start(desc: DragDesc, events: Sender<DragEvent>) -> bool {
    let Some(feed) = desc.staged.clone() else {
        warn!("drag {}: no staging feed", desc.id);
        return false;
    };
    if std::fs::create_dir_all(&feed.root).is_err() {
        return false;
    }
    CANCEL.store(0, Ordering::Release);
    std::thread::Builder::new()
        .name("csc-dragsrc".into())
        .spawn(move || unsafe { run(desc, feed, events) })
        .is_ok()
}

struct Shared {
    id: u64,
    feed: Arc<StagedFeed>,
    events: Mutex<Sender<DragEvent>>,
    announced: AtomicBool,
    async_mode: AtomicBool,
    in_operation: AtomicBool,
    ended: AtomicBool,
}

impl Shared {
    /// Explorer wants the bytes: the core binds the drop to the staging feed.
    fn announce(&self) {
        if !self.announced.swap(true, Ordering::AcqRel) {
            if let Ok(tx) = self.events.lock() {
                let _ = tx.send(DragEvent::Staged { id: self.id });
            }
        }
    }

    fn send(&self, ev: DragEvent) {
        if let Ok(tx) = self.events.lock() {
            let _ = tx.send(ev);
        }
    }
}

unsafe fn run(desc: DragDesc, feed: Arc<StagedFeed>, events: Sender<DragEvent>) {
    let id = desc.id;
    if let Err(e) = OleInitialize(None) {
        warn!("drag {id}: OleInitialize: {e}");
        let _ = events.send(DragEvent::Cancelled { id, reason: "error" });
        return;
    }
    let shared = Arc::new(Shared {
        id,
        feed: feed.clone(),
        events: Mutex::new(events),
        announced: AtomicBool::new(false),
        async_mode: AtomicBool::new(true),
        in_operation: AtomicBool::new(false),
        ended: AtomicBool::new(false),
    });
    let formats = Formats::new();
    let data: IDataObject = DataObject {
        shared: shared.clone(),
        items: desc.items,
        formats,
    }
    .into();
    let source: IDropSource = DropSource { id }.into();
    let mut effect = DROPEFFECT_NONE;
    let hr = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
    debug!("drag {id}: DoDragDrop {hr:?}, effect {}", effect.0);
    if hr == DRAGDROP_S_DROP && (effect != DROPEFFECT_NONE || shared.announced.load(Ordering::Acquire)) {
        shared.announce();
        // Explorer calls back into this apartment while it copies.
        let started = Instant::now();
        loop {
            pump();
            let extracting = shared.in_operation.load(Ordering::Acquire) && !shared.ended.load(Ordering::Acquire);
            let feeding = !feed.all_done() && !feed.is_failed();
            if (!extracting && !feeding) || started.elapsed() > EXTRACT_LIMIT {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        // Explorer may still hold the stream for a moment after the last byte.
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_secs(2) {
            pump();
            std::thread::sleep(Duration::from_millis(15));
        }
    } else {
        let reason = if CANCEL.load(Ordering::Acquire) == id {
            "left screen"
        } else if hr == DRAGDROP_S_CANCEL {
            "cancelled"
        } else {
            "no target"
        };
        shared.send(DragEvent::Cancelled { id, reason });
    }
    drop(data);
    drop(source);
    OleUninitialize();
    if feed.all_done() || feed.is_failed() || !shared.announced.load(Ordering::Acquire) {
        let _ = std::fs::remove_dir_all(&feed.root);
    }
    info!("drag {id}: source finished");
}

unsafe fn pump() {
    let mut msg = MSG::default();
    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
}

impl Formats {
    unsafe fn new() -> Self {
        Self {
            descriptor: RegisterClipboardFormatW(w!("FileGroupDescriptorW")) as u16,
            contents: RegisterClipboardFormatW(w!("FileContents")) as u16,
        }
    }

    fn list(&self) -> [FORMATETC; 2] {
        [
            FORMATETC {
                cfFormat: self.descriptor,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            },
            FORMATETC {
                cfFormat: self.contents,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_ISTREAM.0 as u32,
            },
        ]
    }
}

fn filetime(unix: i64) -> FILETIME {
    let t = (unix.max(0) as u64 + 11_644_473_600) * 10_000_000;
    FILETIME {
        dwLowDateTime: t as u32,
        dwHighDateTime: (t >> 32) as u32,
    }
}

fn descriptor(item: &DragItem) -> FILEDESCRIPTORW {
    let mut file_name = [0u16; 260];
    let name: Vec<u16> = item.relative_path.replace('/', "\\").encode_utf16().collect();
    let n = name.len().min(file_name.len() - 1);
    file_name[..n].copy_from_slice(&name[..n]);
    FILEDESCRIPTORW {
        dwFlags: FD_ATTRIBUTES | FD_WRITESTIME | FD_FILESIZE | FD_PROGRESSUI | FD_UNICODE,
        dwFileAttributes: if item.is_directory {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        },
        ftLastWriteTime: filetime(item.mtime_unix),
        nFileSizeHigh: (item.size >> 32) as u32,
        nFileSizeLow: item.size as u32,
        cFileName: file_name,
        ..Default::default()
    }
}

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct DataObject {
    shared: Arc<Shared>,
    items: Vec<DragItem>,
    formats: Formats,
}

impl DataObject {
    unsafe fn group_descriptor(&self) -> Result<HGLOBAL> {
        let n = self.items.len();
        let size = std::mem::size_of::<u32>() + n * std::mem::size_of::<FILEDESCRIPTORW>();
        let h = GlobalAlloc(GMEM_MOVEABLE, size)?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            return Err(E_UNEXPECTED.into());
        }
        (p as *mut u32).write_unaligned(n as u32);
        let list = p.add(std::mem::size_of::<u32>()) as *mut FILEDESCRIPTORW;
        for (i, item) in self.items.iter().enumerate() {
            list.add(i).write_unaligned(descriptor(item));
        }
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        let fmt = unsafe { pformatetcin.as_ref() }.ok_or_else(|| Error::from(DV_E_FORMATETC))?;
        if fmt.cfFormat == self.formats.descriptor && fmt.tymed & TYMED_HGLOBAL.0 as u32 != 0 {
            let h = unsafe { self.group_descriptor()? };
            return Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: h },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }
        if fmt.cfFormat == self.formats.contents && fmt.tymed & TYMED_ISTREAM.0 as u32 != 0 {
            let index = usize::try_from(fmt.lindex).map_err(|_| Error::from(DV_E_FORMATETC))?;
            let item = self.items.get(index).ok_or_else(|| Error::from(DV_E_FORMATETC))?;
            if item.is_directory {
                return Err(DV_E_FORMATETC.into());
            }
            self.shared.announce();
            let stream: IStream = FeedStream {
                feed: self.shared.feed.clone(),
                index,
                pos: AtomicU64::new(0),
                mtime: item.mtime_unix,
            }
            .into();
            return Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 {
                    pstm: ManuallyDrop::new(Some(stream)),
                },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }
        Err(DV_E_FORMATETC.into())
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let Some(fmt) = (unsafe { pformatetc.as_ref() }) else {
            return DV_E_FORMATETC;
        };
        let ok = self
            .formats
            .list()
            .iter()
            .any(|f| f.cfFormat == fmt.cfFormat && f.tymed & fmt.tymed != 0);
        if ok {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        E_NOTIMPL
    }

    fn SetData(&self, _pformatetc: *const FORMATETC, pmedium: *const STGMEDIUM, frelease: BOOL) -> Result<()> {
        // Explorer reports drop effects and drag images here; none are needed.
        if frelease.as_bool() && !pmedium.is_null() {
            unsafe { ReleaseStgMedium(pmedium as *mut STGMEDIUM) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        unsafe { SHCreateStdEnumFmtEtc(&self.formats.list()) }
    }

    fn DAdvise(&self, _f: *const FORMATETC, _advf: u32, _sink: Option<&IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for DataObject_Impl {
    fn SetAsyncMode(&self, fdoopasync: BOOL) -> Result<()> {
        self.shared.async_mode.store(fdoopasync.as_bool(), Ordering::Release);
        Ok(())
    }

    fn GetAsyncMode(&self) -> Result<BOOL> {
        Ok(self.shared.async_mode.load(Ordering::Acquire).into())
    }

    fn StartOperation(&self, _pbcreserved: Option<&IBindCtx>) -> Result<()> {
        self.shared.in_operation.store(true, Ordering::Release);
        self.shared.announce();
        Ok(())
    }

    fn InOperation(&self) -> Result<BOOL> {
        Ok(self.shared.in_operation.load(Ordering::Acquire).into())
    }

    fn EndOperation(&self, hresult: HRESULT, _pbcreserved: Option<&IBindCtx>, _dweffects: u32) -> Result<()> {
        debug!("drag {}: Explorer finished copying ({hresult:?})", self.shared.id);
        self.shared.ended.store(true, Ordering::Release);
        Ok(())
    }
}

#[implement(IDropSource)]
struct DropSource {
    id: u64,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() || CANCEL.load(Ordering::Acquire) == self.id {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// One staged file as Explorer reads it, waiting for bytes still in transit.
#[implement(IStream)]
struct FeedStream {
    feed: Arc<StagedFeed>,
    index: usize,
    pos: AtomicU64,
    mtime: i64,
}

impl FeedStream {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let path = self.feed.path(self.index).ok_or(std::io::ErrorKind::NotFound)?;
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(pos))?;
        f.read(buf)
    }
}

impl ISequentialStream_Impl for FeedStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let set_read = |n: u32| {
            if let Some(r) = unsafe { pcbread.as_mut() } {
                *r = n;
            }
        };
        set_read(0);
        let size = self.feed.size(self.index);
        let pos = self.pos.load(Ordering::Acquire);
        if pos >= size || cb == 0 {
            return S_FALSE;
        }
        let available = match self.feed.wait(self.index, pos, STALL_LIMIT) {
            Wait::Ready(w) if w > pos => w,
            Wait::Ready(_) => return S_FALSE,
            Wait::Failed | Wait::TimedOut => return E_UNEXPECTED,
        };
        let want = (cb as u64).min(available - pos) as usize;
        let buf = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, want) };
        match self.read_at(pos, buf) {
            Ok(n) => {
                self.pos.store(pos + n as u64, Ordering::Release);
                set_read(n as u32);
                if n == 0 {
                    S_FALSE
                } else {
                    S_OK
                }
            }
            Err(_) => E_UNEXPECTED,
        }
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        E_NOTIMPL
    }
}

impl IStream_Impl for FeedStream_Impl {
    fn Seek(&self, dlibmove: i64, dworigin: STREAM_SEEK, plibnewposition: *mut u64) -> Result<()> {
        let base = match dworigin {
            STREAM_SEEK_SET => 0i64,
            STREAM_SEEK_CUR => self.pos.load(Ordering::Acquire) as i64,
            STREAM_SEEK_END => self.feed.size(self.index) as i64,
            _ => return Err(E_UNEXPECTED.into()),
        };
        let to = base.checked_add(dlibmove).filter(|p| *p >= 0).ok_or_else(|| Error::from(E_UNEXPECTED))? as u64;
        self.pos.store(to, Ordering::Release);
        if let Some(out) = unsafe { plibnewposition.as_mut() } {
            *out = to;
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn CopyTo(&self, _pstm: Option<&IStream>, _cb: u64, _pcbread: *mut u64, _pcbwritten: *mut u64) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> Result<()> {
        Ok(())
    }

    fn Revert(&self) -> Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> Result<()> {
        let out = unsafe { pstatstg.as_mut() }.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        *out = STATSTG {
            r#type: STGTY_STREAM,
            cbSize: self.feed.size(self.index),
            mtime: filetime(self.mtime),
            ..Default::default()
        };
        Ok(())
    }

    fn Clone(&self) -> Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}
