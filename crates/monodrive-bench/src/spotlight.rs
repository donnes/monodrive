//! Spotlight through `MDQuery`, in this process, as an app such as Finder
//! would ask it. `mdfind` takes about 50 ms even for no match, and that cost
//! isn't Spotlight's engine, so only this path is compared with fsearch.

use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::ptr;

type CfRef = *const c_void;

const UTF8: u32 = 0x0800_0100;
const SYNCHRONOUS: usize = 1;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFArrayCreate(
        alloc: CfRef,
        values: *const CfRef,
        count: isize,
        callbacks: *const c_void,
    ) -> CfRef;
    fn CFRelease(value: CfRef);
    fn CFStringCreateWithCString(alloc: CfRef, text: *const c_char, encoding: u32) -> CfRef;
    fn CFStringGetLength(text: CfRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(text: CfRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    static kMDItemPath: CfRef;
    fn MDQueryCreate(alloc: CfRef, query: CfRef, values: CfRef, sorting: CfRef) -> CfRef;
    fn MDQuerySetSearchScope(query: CfRef, scope: CfRef, options: u32);
    fn MDQueryExecute(query: CfRef, options: usize) -> u8;
    fn MDQueryGetResultCount(query: CfRef) -> isize;
    fn MDQueryGetResultAtIndex(query: CfRef, index: isize) -> CfRef;
    fn MDItemCopyAttribute(item: CfRef, name: CfRef) -> CfRef;
}

/// A Core Foundation object this code owns.
struct Owned(CfRef);

impl Owned {
    fn new(value: CfRef, what: &str) -> Result<Self, String> {
        if value.is_null() {
            Err(format!("{what} failed"))
        } else {
            Ok(Self(value))
        }
    }

    fn string(text: &str) -> Result<Self, String> {
        let text = CString::new(text).map_err(|error| error.to_string())?;
        // SAFETY: a NUL-terminated string, copied by the call.
        let value = unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) };
        Self::new(value, "CFStringCreateWithCString")
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `new` took ownership of a non-null object.
        unsafe { CFRelease(self.0) };
    }
}

fn to_string(text: CfRef) -> Option<String> {
    // SAFETY: `text` is a live CFString, and the buffer is as large as the
    // size the call before it reports.
    unsafe {
        let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(text), UTF8) + 1;
        let mut buffer = vec![0u8; usize::try_from(size).ok()?];
        (CFStringGetCString(text, buffer.as_mut_ptr().cast(), size, UTF8) != 0).then_some(())?;
        let end = buffer.iter().position(|b| *b == 0)?;
        buffer.truncate(end);
        String::from_utf8(buffer).ok()
    }
}

/// `MDQuery`'s language, which `mdfind` takes too.
pub fn predicate(words: &[&str]) -> String {
    words
        .iter()
        .map(|word| format!("kMDItemFSName == \"*{word}*\"cd"))
        .collect::<Vec<_>>()
        .join(" && ")
}

/// Every path under `root` matching `predicate`.
pub fn search(root: &Path, predicate: &str) -> Result<Vec<String>, String> {
    let scope_path = Owned::string(&root.to_string_lossy())?;
    let text = Owned::string(predicate)?;
    // SAFETY: each pointer is a live object for the call, a null allocator
    // and null option arrays are the documented defaults, and each returned
    // object is owned by `Owned` or, for results, by the query.
    unsafe {
        let scope_paths = [scope_path.0];
        let scope = Owned::new(
            CFArrayCreate(
                ptr::null(),
                scope_paths.as_ptr(),
                1,
                &raw const kCFTypeArrayCallBacks,
            ),
            "CFArrayCreate",
        )?;
        let query = Owned::new(
            MDQueryCreate(ptr::null(), text.0, ptr::null(), ptr::null()),
            "MDQueryCreate",
        )?;
        MDQuerySetSearchScope(query.0, scope.0, 0);
        if MDQueryExecute(query.0, SYNCHRONOUS) == 0 {
            return Err("MDQueryExecute failed".to_string());
        }
        let count = MDQueryGetResultCount(query.0);
        let mut paths = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for index in 0..count {
            let item = MDQueryGetResultAtIndex(query.0, index);
            let path = Owned::new(
                MDItemCopyAttribute(item, kMDItemPath),
                "MDItemCopyAttribute",
            );
            // An item deleted a moment ago has no path, and isn't a result.
            if let Some(path) = path.ok().and_then(|path| to_string(path.0)) {
                paths.push(path);
            }
        }
        Ok(paths)
    }
}
