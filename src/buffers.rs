//! Buffer allocation: wl_shm (CPU, used for the small UI panel) and GBM dmabufs (GPU, used for
//! live previews so window contents never leave video memory).

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rustix::fs::{ftruncate, memfd_create, MemfdFlags};
use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};

/// Set while the compositor holds the buffer (between attach+commit and wl_buffer.release).
#[derive(Clone, Default)]
pub struct BusyFlag(pub Arc<AtomicBool>);

impl BusyFlag {
    pub fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn set(&self, v: bool) {
        self.0.store(v, Ordering::Release)
    }
}

pub struct Mmap {
    ptr: *mut u8,
    len: usize,
}

impl Mmap {
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.ptr.cast(), self.len);
        }
    }
}

/// Anonymous shared memory file of `size` bytes, optionally mapped into our address space.
pub fn shm_file(size: usize, map: bool) -> io::Result<(OwnedFd, Option<Mmap>)> {
    let fd = memfd_create("pivot-shm", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    ftruncate(&fd, size as u64)?;
    let m = if map {
        let ptr = unsafe {
            mmap(std::ptr::null_mut(), size, ProtFlags::READ | ProtFlags::WRITE, MapFlags::SHARED, &fd, 0)?
        };
        Some(Mmap { ptr: ptr.cast(), len: size })
    } else {
        None
    };
    Ok((fd, m))
}

pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub struct DmabufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

pub struct Dmabuf {
    pub planes: Vec<DmabufPlane>,
    pub modifier: u64,
}

/// GBM allocator bound to the DRM render node the compositor asked us to allocate on.
pub struct GbmAllocator {
    device: gbm::Device<File>,
}

impl GbmAllocator {
    pub fn open(dev: u64) -> io::Result<Self> {
        let path = render_node_for(dev)?;
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
        let device = gbm::Device::new(file)?;
        log::info!("allocating preview buffers on {}", path.display());
        Ok(Self { device })
    }

    pub fn alloc(&self, width: u32, height: u32, fourcc: u32, modifiers: &[u64]) -> io::Result<Dmabuf> {
        let format = gbm::Format::try_from(fourcc)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("unknown fourcc {fourcc:#x}")))?;
        let explicit: Vec<gbm::Modifier> =
            modifiers.iter().copied().filter(|&m| m != DRM_FORMAT_MOD_INVALID).map(gbm::Modifier::from).collect();
        let implicit_ok = modifiers.is_empty() || modifiers.contains(&DRM_FORMAT_MOD_INVALID);
        let flags = gbm::BufferObjectFlags::RENDERING;

        let mut attempt: io::Result<gbm::BufferObject<()>> =
            Err(io::Error::new(io::ErrorKind::Unsupported, "no usable modifier"));
        if !explicit.is_empty() {
            attempt = self
                .device
                .create_buffer_object_with_modifiers2(width, height, format, explicit.iter().copied(), flags)
                .or_else(|_| {
                    self.device.create_buffer_object_with_modifiers(width, height, format, explicit.iter().copied())
                });
        }
        if attempt.is_err() && implicit_ok {
            attempt = self.device.create_buffer_object(width, height, format, flags);
        }
        let bo = attempt?;
        let modifier: u64 = if explicit.is_empty() { DRM_FORMAT_MOD_INVALID } else { bo.modifier().into() };
        let mut planes = Vec::new();
        for i in 0..bo.plane_count() as i32 {
            let fd = bo.fd_for_plane(i).map_err(|e| io::Error::other(format!("gbm plane fd: {e}")))?;
            planes.push(DmabufPlane { fd, offset: bo.offset(i), stride: bo.stride_for_plane(i) });
        }
        // The exported fds keep the memory alive; the compositor takes its own references once
        // the wl_buffer is created, so the bo itself can go.
        drop(bo);
        Ok(Dmabuf { planes, modifier })
    }
}

/// Map a dev_t (as sent in `dmabuf_device`) to its /dev/dri/renderD* node via sysfs. Works whether
/// the compositor advertised the primary (card) node or the render node.
fn render_node_for(dev: u64) -> io::Result<PathBuf> {
    let (maj, min) = (rustix::fs::major(dev), rustix::fs::minor(dev));
    let sys = PathBuf::from(format!("/sys/dev/char/{maj}:{min}/device/drm"));
    if let Ok(rd) = std::fs::read_dir(&sys) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("renderD") {
                return Ok(PathBuf::from("/dev/dri").join(&*name));
            }
        }
    }
    // Fall back to scanning /dev/dri for a node with the same dev_t.
    for e in std::fs::read_dir("/dev/dri")?.flatten() {
        if let Ok(st) = rustix::fs::stat(e.path()) {
            #[allow(clippy::unnecessary_cast)]
            if st.st_rdev as u64 == dev {
                return Ok(e.path());
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("no DRM node for device {maj}:{min}")))
}

/// Pick the best format the compositor offers for previews. Alpha formats first so client-side
/// rounded corners/shadows stay transparent.
pub fn pick_dmabuf_format(formats: &[(u32, Vec<u64>)]) -> Option<&(u32, Vec<u64>)> {
    const PREF: [u32; 4] = [
        fourcc(b"AR24"), // ARGB8888
        fourcc(b"XR24"), // XRGB8888
        fourcc(b"AB24"), // ABGR8888
        fourcc(b"XB24"), // XBGR8888
    ];
    PREF.iter().find_map(|p| formats.iter().find(|(f, _)| f == p)).or_else(|| formats.first())
}

pub const fn fourcc(c: &[u8; 4]) -> u32 {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | (c[3] as u32) << 24
}

/// Read a native-endian array of `T` from a wayland `array` argument.
pub fn ne_u32s(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c))
}

pub fn ne_u64s(bytes: &[u8]) -> impl Iterator<Item = u64> + '_ {
    bytes.as_chunks::<8>().0.iter().map(|c| u64::from_ne_bytes(*c))
}

pub fn dev_t_from_bytes(bytes: &[u8]) -> Option<u64> {
    match bytes.len() {
        8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
        4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
        _ => None,
    }
}
