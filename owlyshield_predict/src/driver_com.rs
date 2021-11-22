//! Low-level communication with the minifilter.

use core::ffi::c_void;
use std::mem;
use std::os::raw::*;
use std::ptr;

use bindings::Windows::Win32::Foundation::CloseHandle;
use bindings::Windows::Win32::Foundation::{HANDLE, PWSTR};
use bindings::Windows::Win32::Storage::InstallableFileSystems::{
    FilterConnectCommunicationPort, FilterSendMessage,
};
use sysinfo::{get_current_pid, Pid};
use wchar::wchar_t;
use widestring::U16CString;
use windows::HRESULT;

use crate::driver_com::shared_def::ReplyIrp;
use crate::driver_com::IrpMajorOp::{IrpCreate, IrpNone, IrpRead, IrpSetInfo, IrpWrite};

type BufPath = [wchar_t; 520];

/// The usermode app (this app) can send several messages types to the driver. See [ComMessageType]
/// for details.
/// Depending on the message type, the *pid*, *gid* and *path* fields can be optional.
#[derive(Debug)]
#[repr(C)]
struct ComMessage {
    /// The type message to send. See [ComMessageType].
    r#type: c_ulong,
    /// The pid of the process which triggered an i/o activity;
    pid: c_ulong,
    /// The gid is maintained by the driver
    gid: c_ulonglong,
    path: BufPath,
}

/// A minifilter is identified by a port (know in advance), like a named pipe used for communication,
/// and a handle, retrieved by [Self::open_kernel_driver_com].
#[derive(Debug)]
pub struct Driver {
    com_port_name: *mut u16,
    handle: HANDLE,
}

/// Messages types to send directives to the minifilter, by using te [ComMessage] struct.
enum ComMessageType {
    /// Not used yet. The minifilter has the ability to monitor a specific part of the fs.
    MessageAddScanDirectory,
    /// Not used yet. The minifilter has the ability to monitor a specific part of the fs.
    MessageRemScanDirectory,
    /// Ask for a [ReplyIrp], if any available.
    MessageGetOps,
    /// Set this app pid to the minifilter (related IRPs will be ignored);
    MessageSetPid,
    /// Instruct the minifilter to kill all pids in the family designated by a given gid.
    MessageKillGid,
}

/// See [shared_def::DriverMsg] struct.
pub enum IrpMajorOp {
    IrpNone,
    IrpRead,
    IrpWrite,
    IrpSetInfo,
    IrpCreate,
    IrpCleanUp,
}

impl IrpMajorOp {
    pub fn from_byte(b: u8) -> IrpMajorOp {
        match b {
            0 => IrpNone,
            1 => IrpRead,
            2 => IrpWrite,
            3 => IrpSetInfo,
            4 => IrpCreate,
            5 => IrpCreate,
            _ => IrpNone,
        }
    }
}

impl Driver {
    /// Can be used to properly close the communication (and unregister) with the minifilter.
    /// If this fn is not used and the program has stopped, the handle is automatically closed,
    /// seemingly without any side-effects.
    pub fn close_kernel_communication(&self) -> bool {
        unsafe { CloseHandle(&self.handle).as_bool() }
    }

    /// The usermode running app (this one) has to register itself to the driver.
    pub fn driver_set_app_pid(&self) -> Result<(), windows::Error> {
        let buf = Driver::string_to_commessage_buffer(r"\Device\harddiskVolume");

        let mut get_irp_msg: ComMessage = ComMessage {
            r#type: ComMessageType::MessageSetPid as c_ulong, //MessageSetPid
            pid: get_current_pid().unwrap() as c_ulong,
            gid: 140713315094899,
            path: buf, //wch!("\0"),
        };
        let mut tmp: u32 = 0;
        unsafe {
            FilterSendMessage(
                self.handle,
                ptr::addr_of_mut!(get_irp_msg) as *mut c_void,
                mem::size_of::<ComMessage>() as c_ulong,
                ptr::null_mut(),
                0,
                &mut tmp as *mut u32,
            )
        }
    }

    /// Try to open a com canal with the minifilter before this app is registered. This fn can fail
    /// is the minifilter is unreachable:
    /// * if it is not started (try ```sc start owlyshieldransomfilter``` first
    /// * if a connection is already established: it can accepts only one at a time.
    /// In that case the Error is raised by the OS (windows::Error) and is generally readable.
    pub fn open_kernel_driver_com() -> Result<Driver, windows::Error> {
        let _com_port_name = U16CString::from_str("\\RWFilter").unwrap().into_raw();
        let _handle;
        unsafe {
            _handle = FilterConnectCommunicationPort(
                PWSTR(_com_port_name),
                0,
                ptr::null(),
                0,
                ptr::null_mut(),
            )?
        }
        let res = Driver {
            com_port_name: _com_port_name,
            handle: _handle,
        };
        Ok(res)
    }

    /// Ask the driver for a [ReplyIrp], if any. This is a low-level function and the returned object
    /// uses C pointers. Managing C pointers requires a special care, because of the Rust timelines.
    /// [ReplyIrp] is optional since the minifilter returns null if there is no new activity.
    pub fn get_irp(&self, vecnew: &mut Vec<u8>) -> Option<ReplyIrp> {
        let mut get_irp_msg = Driver::build_irp_msg(
            ComMessageType::MessageGetOps,
            get_current_pid().unwrap(),
            0,
            "",
        );
        let mut tmp: u32 = 0;
        unsafe {
            FilterSendMessage(
                self.handle,
                ptr::addr_of_mut!(get_irp_msg) as *mut c_void,
                mem::size_of::<ComMessage>() as c_ulong,
                vecnew.as_ptr() as *mut c_void,
                65536 as u32,
                ptr::addr_of_mut!(tmp) as *mut u32,
            )
            .expect("Cannot get driver message from driver");
        }
        if tmp != 0 {
            let reply_irp: shared_def::ReplyIrp;
            unsafe {
                reply_irp = std::ptr::read_unaligned(vecnew.as_ptr() as *const ReplyIrp);
            }
            return Some(reply_irp);
        }
        None
    }

    /// Ask the minifilter to kill all pids related to the given *gid*. Pids are killed in drivermode
    /// by calls to NtClose.
    pub fn try_kill(&self, gid: c_ulonglong) -> Result<HRESULT, windows::Error> {
        let mut killmsg = ComMessage {
            r#type: ComMessageType::MessageKillGid as c_ulong,
            pid: 0, //get_current_pid().unwrap() as u32,
            gid: gid,
            path: [0; 520],
        };
        let mut res: u32 = 0;
        let mut res_size: u32 = 0;

        unsafe {
            FilterSendMessage(
                self.handle,
                ptr::addr_of_mut!(killmsg) as *mut c_void,
                mem::size_of::<ComMessage>() as c_ulong,
                ptr::addr_of_mut!(res) as *mut c_void,
                4 as u32,
                ptr::addr_of_mut!(res_size) as *mut u32,
            )?;
        }
        //TODO

        let hres = HRESULT(res);
        return Ok(hres);
    }

    fn string_to_commessage_buffer(bufstr: &str) -> BufPath {
        let temp = U16CString::from_str(&bufstr).unwrap();
        let mut buf: BufPath = [0; 520];
        for (i, c) in temp.as_slice_with_nul().iter().enumerate() {
            buf[i] = c.clone() as wchar_t;
        }
        buf
    }

    // TODO: move to ComMessage?
    fn build_irp_msg(commsgtype: ComMessageType, pid: Pid, gid: u64, path: &str) -> ComMessage {
        ComMessage {
            r#type: commsgtype as c_ulong, //MessageSetPid
            pid: pid as c_ulong,
            gid: gid,
            path: Driver::string_to_commessage_buffer(&path),
        }
    }
}

/// Contains all definitions shared between this usermode app and the minifilter in order
/// to communicate properly. Those are C-representation of structures sent or received from the minifilter.
pub mod shared_def {
    use std::os::raw::{c_uchar, c_ulong, c_ulonglong, c_ushort};
    use std::path::PathBuf;

    use bindings::Windows::Win32::Storage::FileSystem::FILE_ID_INFO;
    use serde::{Deserialize, Serialize};
    use wchar::wchar_t;

    /// See [DriverMsg] struct.
    #[derive(FromPrimitive)]
    pub enum FileChangeInfo {
        FileChangeNotSet,
        FileOpenDirectory,
        FileChangeWrite,
        FileChangeNewFile,
        FileChangeRenameFile,
        FileChangeExtensionChanged,
        FileChangeDeleteFile,
        FileChangeDeleteNewFile, //TODO
        FileChangeOverwriteFile,
    }

    /// See [DriverMsg] struct.
    #[derive(FromPrimitive)]
    pub enum FileLocationInfo {
        FileNotProtected,
        FileProtected,
        FileMovedIn,
        FileMovedOut,
    }

    /// Low-level C-like object to communicate with the minifilter.
    /// The minifilter yields ReplyIrp objects (retrieved by [crate::driver_com::Driver::get_irp] to manage the fixed size of the *data buffer.
    /// In other words, a ReplyIrp is a collection of [CDriverMsg] with a capped size.
    #[derive(Debug, Copy, Clone)]
    #[repr(C)]
    pub struct ReplyIrp {
        /// The size od the collection.
        pub data_size: c_ulonglong,
        /// The C pointer to the buffer containinf the [CDriverMsg] events.
        pub data: *const CDriverMsg,
        /// The number of different operations in this collection.
        pub num_ops: u64,
    }

    /// This class is the straight Rust translation of the Win32 API [UNICODE_STRING](https://docs.microsoft.com/en-us/windows/win32/api/ntdef/ns-ntdef-_unicode_string),
    /// returned by the driver.
    #[derive(Debug, Copy, Clone)]
    #[repr(C)]
    pub struct UnicodeString {
        pub length: c_ushort,
        pub maximum_length: c_ushort,
        pub buffer: *const wchar_t,
    }

    /// Represents a driver message.
    ///
    /// - extension: The file extension
    /// - file_id_vsn: Hard Disk Volume Serial Number where the file is saved (from FILE_ID_INFO)
    /// - file_id_id:  File ID on the disk (FILE_ID_INFO)
    /// - mem_size_used: Number of bytes transferred (IO_STATUS_BLOCK.Information)
    /// - entropy: (Optional) File Entropy calculated by the driver
    /// - is_entropy_calc: is the entropy calculated?
    /// - pid: Pid responsible for this io activity
    /// - irp_op: Windows IRP Type catched by the minifilter:
    ///     * NONE (0)
    ///     * READ (1)
    ///     * WRITE (2)
    ///     * SETINFO (3)
    ///     * CREATE (4)
    ///     * CLEANUP (5)
    /// - file_change: type of i/o operation:
    ///     * FILE_CHANGE_NOT_SET (0)
    ///     * FILE_OPEN_DIRECTORY (1)
    ///     * FILE_CHANGE_WRITE (2)
    ///     * FILE_CHANGE_NEW_FILE (3)
    ///     * FILE_CHANGE_RENAME_FILE (4)
    ///     * FILE_CHANGE_EXTENSION_CHANGED (5)
    ///     * FILE_CHANGE_DELETE_FILE (6)
    ///     * FILE_CHANGE_DELETE_NEW_FILE (7)
    ///     * FILE_CHANGE_OVERWRITE_FILE (8)
    /// - file_location_info: the driver has the ability to monitor specific directories only (feature currently not used):
    ///     * FILE_NOT_PROTECTED (0): Monitored dirs do not contained this file
    ///     * FILE_PROTECTED (1)
    ///     * FILE_MOVED_IN (2)
    ///     * FILE_MOVED_OUT (3)
    /// - filepath: File path on the disk
    /// - gid: Group Identifier (maintained by the minifilter) of the operation
    /// - runtime_features: see class [RuntimeFeatures]
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[repr(C)]
    pub struct DriverMsg {
        pub extension: [wchar_t; 12],
        pub file_id_vsn: c_ulonglong,
        pub file_id_id: [u8; 16],
        pub mem_sized_used: c_ulonglong,
        pub entropy: f64,
        pub pid: c_ulong,
        pub irp_op: c_uchar,
        pub is_entropy_calc: u8,
        pub file_change: c_uchar,
        pub file_location_info: c_uchar,
        pub filepathstr: String,
        pub gid: c_ulonglong,
        pub runtime_features: RuntimeFeatures,
    }

    /// Stores runtime features that come from *owlyshield_predict* (and not the minifilter).
    ///
    /// - exepath: The path of the gid root process
    /// - exe_exists: Did the root exe file still existed (at the moment of this specific *DriverMessage* operation)?
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RuntimeFeatures {
        pub exepath: PathBuf,
        pub exe_still_exists: bool,
    }

    /// The C object returned by the minifilter, available through [ReplyIrp].
    /// It is low level and use C pointers logic which is
    /// not always compatible with RUST (in particular the lifetime of *next). That's why we convert
    /// it asap to a plain Rust [DriverMsg] object.
    /// ```next``` is null (0x0) when there is no [DriverMsg] remaining
    #[derive(Debug, Copy, Clone)]
    #[repr(C)]
    pub struct CDriverMsg {
        pub extension: [wchar_t; 12],
        pub file_id: FILE_ID_INFO,
        pub mem_sized_used: c_ulonglong,
        pub entropy: f64,
        pub pid: c_ulong,
        pub irp_op: c_uchar,
        pub is_entropy_calc: u8,
        pub file_change: c_uchar,
        pub file_location_info: c_uchar,
        pub filepath: UnicodeString,
        pub gid: c_ulonglong,
        /// null (0x0) when there is no [DriverMsg] remaining
        pub next: *const CDriverMsg,
    }

    /// To iterate easily over a collection of [CDriverMsg] received from the minifilter, before they
    /// are converted to [DriverMsg]
    pub struct CDriverMsgs<'a> {
        drivermsgs: Vec<&'a CDriverMsg>,
        index: usize,
    }

    impl UnicodeString {
        pub fn to_string(&self) -> String {
            unsafe {
                let str_slice = std::slice::from_raw_parts(self.buffer, self.length as usize);
                let mut first_zero_index = 0;
                for (i, c) in str_slice.iter().enumerate() {
                    if *c == 0 {
                        first_zero_index = i;
                        break;
                    }
                }
                String::from_utf16_lossy(&str_slice[..first_zero_index])
            }
        }
    }

    impl ReplyIrp {
        /// Iterate through ```self.data``` and returns the collection of [CDriverMsg]
        fn unpack_drivermsg(&self) -> Vec<&CDriverMsg> {
            let mut res = vec![];
            unsafe {
                let mut msg = &*self.data;
                res.push(msg);
                for _ in 1..(self.num_ops) {
                    if msg.next.is_null() {
                        break;
                    }
                    msg = &*msg.next;
                    res.push(msg);
                }
            }
            res
        }
    }

    impl DriverMsg {
        pub fn from(c_drivermsg: &CDriverMsg) -> DriverMsg {
            DriverMsg {
                extension: c_drivermsg.extension,
                file_id_vsn: c_drivermsg.file_id.VolumeSerialNumber,
                file_id_id: c_drivermsg.file_id.FileId.Identifier,
                mem_sized_used: c_drivermsg.mem_sized_used,
                entropy: c_drivermsg.entropy,
                pid: c_drivermsg.pid,
                irp_op: c_drivermsg.irp_op,
                is_entropy_calc: c_drivermsg.is_entropy_calc,
                file_change: c_drivermsg.file_change,
                file_location_info: c_drivermsg.file_location_info,
                filepathstr: c_drivermsg.filepath.to_string(),
                gid: c_drivermsg.gid,
                runtime_features: RuntimeFeatures::new(),
            }
        }
    }

    impl RuntimeFeatures {
        pub fn new() -> RuntimeFeatures {
            RuntimeFeatures {
                exepath: PathBuf::new(),
                exe_still_exists: true,
            }
        }
    }

    impl CDriverMsgs<'_> {
        pub fn new(irp: &ReplyIrp) -> CDriverMsgs {
            CDriverMsgs {
                drivermsgs: irp.unpack_drivermsg(),
                index: 0,
            }
        }
    }

    impl Iterator for CDriverMsgs<'_> {
        type Item = CDriverMsg;

        fn next(&mut self) -> Option<Self::Item> {
            if self.index == self.drivermsgs.len() {
                None
            } else {
                let res = *self.drivermsgs[self.index];
                self.index += 1;
                Some(res)
            }
        }
    }
}
