//! One-time compatibility gate for the private EMF upload-reuse candidate.
//! Disk/hash work runs once on the first EMF view, never per draw/frame.
//! Unknown addon builds fail closed; keep this private inspected-package gate.
use sha2::{Digest, Sha256};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
const RESHADE_SHA: &str = "da430e0a9c6eecefa0d1b27d05e16c426fb5d04e808b194d914eaac4b31bc0f8";
const RENODX_SHA: &str = "1f3a1aab0d4fa3c884341adc8e0b25af940da1f0b14a2cdf443f1f280a8c4c66";
#[link(name="kernel32")]
extern "system" {
 fn GetCurrentProcess() -> isize;
 fn GetSystemDirectoryW(path:*mut u16, capacity:u32) -> u32;
 fn GetSystemWow64DirectoryW(path:*mut u16, capacity:u32) -> u32;
 fn K32EnumProcessModules(process:isize, modules:*mut usize, bytes:u32, needed:*mut u32) -> i32;
 fn GetModuleFileNameW(module:isize, path:*mut u16, capacity:u32) -> u32;
}
unsafe fn module_path(module:usize) -> Option<PathBuf> {
 let mut path=[0u16;32768];let n=GetModuleFileNameW(module as isize,path.as_mut_ptr(),path.len() as u32)as usize;
 (n>0 && n<path.len()).then(||PathBuf::from(OsString::from_wide(&path[..n])))
}
fn digest_matches(path:&std::path::Path,expected:&str)->bool {
 let Ok(bytes)=std::fs::read(path)else{return false};format!("{:x}",Sha256::digest(bytes))==expected
}
fn same_directory(path:&std::path::Path,dir:&std::path::Path)->bool {
 path.parent().is_some_and(|p|p.as_os_str().to_string_lossy().eq_ignore_ascii_case(&dir.as_os_str().to_string_lossy()))
}
// Only reject uninspected graphics entry-point wrappers. This does NOT establish
// a fully native process and MUST NOT relax the two inspected addon hashes.
fn graphics_entry(name:&str)->bool {
 matches!(name,"dxgi.dll"|"d3d11.dll"|"d3d10.dll"|"d3d10_1.dll"|"d3d10core.dll"
  |"d3d9.dll"|"opengl32.dll"|"vulkan-1.dll")
}
unsafe fn system_directory(wow64:bool)->Option<PathBuf> {
 let mut path=[0u16;32768];
 let n=if wow64 {GetSystemWow64DirectoryW(path.as_mut_ptr(),path.len()as u32)}
  else {GetSystemDirectoryW(path.as_mut_ptr(),path.len()as u32)}as usize;
 (n>0 && n<path.len()).then(||PathBuf::from(OsString::from_wide(&path[..n])))
}
fn addon_name(name:&str)->bool {
 name.ends_with(".addon") || name.ends_with(".addon32") || name.ends_with(".addon64")
}
pub(super) unsafe fn verified() -> bool {
 let Some(exe)=module_path(0)else{return false};let Some(dir)=exe.parent()else{return false};
 let Some(system)=system_directory(false)else{return false};let wow64=system_directory(true);
 let mut modules=[0usize;512];let mut needed=0;
 if K32EnumProcessModules(GetCurrentProcess(),modules.as_mut_ptr(),std::mem::size_of_val(&modules)as u32,&mut needed)==0
  || needed as usize>std::mem::size_of_val(&modules) || needed as usize%std::mem::size_of::<usize>()!=0{return false;}
 let mut reshade=false;let mut renodx=false;let mut addons=0;
 for &module in &modules[..needed as usize/std::mem::size_of::<usize>()] {
  let Some(path)=module_path(module)else{return false};
  let Some(name)=path.file_name().and_then(|s|s.to_str()).map(str::to_ascii_lowercase)else{return false};
  let inspected_reshade=name=="dxgi.dll" && same_directory(&path,dir) && digest_matches(&path,RESHADE_SHA);
  // WOW64 may report either OS directory. Never trust a System32 suffix/prefix,
  // game-local D3D proxy, or the presence of an approved pair to bless another wrapper.
  if graphics_entry(&name) && !inspected_reshade && !same_directory(&path,&system)
   && !wow64.as_ref().is_some_and(|p|same_directory(&path,p)){return false;}
  if addon_name(&name) {
   addons+=1;
   if name!="renodx-scblacklist.addon32" || !same_directory(&path,dir) || !digest_matches(&path,RENODX_SHA){return false;}
   renodx=true;
  }
  if name=="dxgi.dll" && same_directory(&path,dir) {reshade=inspected_reshade;}
 }
 reshade && renodx && addons==1
}
#[cfg(test)]mod tests{use super::*;
 #[test]fn all_addon_architectures_are_checked(){for n in ["a.addon","a.addon32","a.addon64"]{assert!(addon_name(n));}assert!(!addon_name("d3d11.dll"));}
 #[test]fn non_game_test_process_is_disabled(){assert!(!unsafe{verified()});}
 #[test]fn graphics_entries_do_not_classify_unrelated_modules(){for n in ["dxgi.dll","d3d11.dll","d3d10.dll","d3d10_1.dll","d3d10core.dll","d3d9.dll","opengl32.dll","vulkan-1.dll"]{assert!(graphics_entry(n));}for n in ["nvwgf2um.dll","uplay_r1_loader.dll","scbl_gameplay.dll","overlay.dll"]{assert!(!graphics_entry(n));}}
 #[test]fn approved_hashes_are_distinct_full_sha256(){assert_eq!(RESHADE_SHA.len(),64);assert_eq!(RENODX_SHA.len(),64);assert_ne!(RESHADE_SHA,RENODX_SHA);}
}
