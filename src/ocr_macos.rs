// macOS OCR：Vision 框架（实现在 src/platform/macos_ocr.mm，Rust 侧只做 FFI 调用）。
use anyhow::Result;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::ocr::{parse_payload, Outcome};

extern "C" {
    fn npa_ocr_recognize(path: *const c_char, lang: *const c_char) -> *mut c_char;
}

pub fn recognize(path: &str, lang: &str) -> Result<Outcome> {
    let c_path = CString::new(path)?;
    let c_lang = CString::new(lang)?;
    let raw_ptr = unsafe { npa_ocr_recognize(c_path.as_ptr(), c_lang.as_ptr()) };
    if raw_ptr.is_null() {
        anyhow::bail!("vision OCR returned no result");
    }
    let json = unsafe { CStr::from_ptr(raw_ptr) }.to_string_lossy().to_string();
    unsafe { libc::free(raw_ptr as *mut libc::c_void) };
    parse_payload(&json)
}
