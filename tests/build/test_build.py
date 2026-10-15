"""Regression tests for the real build entry; no compiler/game/network required."""
from pathlib import Path
import hashlib,importlib.util,json,os,subprocess,sys,tempfile,unittest,zipfile
from unittest.mock import patch

REPO=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(REPO/'scripts'))
spec=importlib.util.spec_from_file_location('scbl_build',REPO/'scripts/build.py')
build=importlib.util.module_from_spec(spec);spec.loader.exec_module(build)

class BuildEntryTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory(prefix='scbl-build-test-')
  self.root=Path(self.tmp.name);self.art=self.root/'artifacts'
  self.patches=[patch.object(build,'ROOT',self.root),patch.object(build,'ART',self.art),patch.object(build,'CONFIG',{})]
  for p in self.patches:p.start()
 def tearDown(self):
  for p in reversed(self.patches):p.stop()
  self.tmp.cleanup()
 def manifest(self,rows):
  p=self.root/'scripts/local-assets.json';p.parent.mkdir(parents=True,exist_ok=True)
  p.write_text(json.dumps({'files':{n:{'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()} for n,b in rows.items()}}))
 def bundle(self,rows):
  p=self.root/'build-assets.zip'
  with zipfile.ZipFile(p,'w') as z:
   for n,b in rows.items():z.writestr(n,b)
  return p
 def test_missing_assets_gives_actionable_error(self):
  self.manifest({'src/a.bin':b'a'})
  with self.assertRaisesRegex(RuntimeError,'build-assets.zip'):build.assets()
 def test_matching_zip_imports_only_allowlist(self):
  self.manifest({'src/a.bin':b'a'});self.bundle({'src/a.bin':b'a','data/private.txt':b'ignored','../escape':b'ignored'})
  build.assets();self.assertEqual((self.root/'src/a.bin').read_bytes(),b'a');self.assertFalse((self.root/'data').exists());self.assertFalse((self.root.parent/'escape').exists())
 def test_invalid_zip_hash_imports_nothing(self):
  self.manifest({'src/a':b'a','src/b':b'b'});self.bundle({'src/a':b'a','src/b':b'c'})
  with self.assertRaisesRegex(RuntimeError,'No files were imported'):build.assets()
  self.assertFalse((self.root/'src').exists())
 def test_existing_mismatched_asset_not_overwritten(self):
  self.manifest({'a':b'a'});(self.root/'a').write_bytes(b'original');self.bundle({'a':b'a'})
  with self.assertRaisesRegex(RuntimeError,'not overwritten'):build.assets()
  self.assertEqual((self.root/'a').read_bytes(),b'original')
 def test_existing_matching_asset_unchanged(self):
  self.manifest({'a':b'a'});p=self.root/'a';p.write_bytes(b'a');before=p.stat().st_mtime_ns
  build.assets();self.assertEqual(p.stat().st_mtime_ns,before)
 def test_directory_import(self):
  self.manifest({'src/a':b'a'});source=self.root/'inputs';(source/'src').mkdir(parents=True);(source/'src/a').write_bytes(b'a')
  build.assets(source);self.assertEqual((self.root/'src/a').read_bytes(),b'a')
 def test_unsafe_manifest_rejected(self):
  self.manifest({'../escape':b'a'})
  with self.assertRaisesRegex(RuntimeError,'Unsafe asset'):build.assets()
 def test_duplicate_zip_asset_rejected(self):
  self.manifest({'a':b'a'});p=self.bundle({'a':b'a'})
  import warnings
  with warnings.catch_warnings():
   warnings.simplefilter('ignore')
   with zipfile.ZipFile(p,'a') as z:z.writestr('a',b'a')
  with self.assertRaisesRegex(RuntimeError,'missing/mismatched'):build.assets()
 def test_path_merge_preserves_protoc_and_git(self):
  self.assertEqual(build.merge_paths(r'C:\VS;C:\Windows',r'C:\Git;C:\Protoc;c:\windows'),r'C:\VS;C:\Windows;C:\Git;C:\Protoc')
 def test_new_output_is_inside_artifacts(self):
  out,z=build.output_paths('release-1');self.assertEqual(out,self.art/'program/release-1');self.assertEqual(z,self.art/'release-1-program.zip')
 def test_repeated_build_preserves_prior_package(self):
  out,z=build.output_paths('release-1');out.mkdir(parents=True);(out/'sentinel').write_text('keep');z.write_bytes(b'old')
  new,nz=build.output_paths('release-1');self.assertNotEqual(new,out);self.assertNotEqual(nz,z);self.assertEqual(z.read_bytes(),b'old');self.assertEqual((out/'sentinel').read_text(),'keep')
 def test_archive_only_collision_preserved(self):
  self.art.mkdir();(self.art/'release-1-program.zip').write_bytes(b'old')
  out,z=build.output_paths('release-1');self.assertNotEqual(out.name,'release-1')
 def test_unsafe_release_rejected(self):
  for value in ['../outside','..','','C:bad','a/b']:
   with self.subTest(value=value),self.assertRaises(RuntimeError):build.output_paths(value)
 def test_all_runs_in_order(self):
  calls=[];self.art.mkdir();(self.art/'BUILD-RESULT.json').write_text('{"archive":"fixture.zip"}')
  with patch.object(build,'doctor',lambda:calls.append('doctor')),patch.object(build,'test',lambda:calls.append('test')),patch.object(build,'build',lambda:calls.append('build')),patch.object(build,'verify_latest',lambda:calls.append('verify')):build.all_steps()
  self.assertEqual(calls,['doctor','test','build','verify'])
 def test_failed_preflight_stops_all_builds(self):
  with patch.object(build,'doctor',side_effect=RuntimeError('missing tools')),patch.object(build,'test') as t,patch.object(build,'build') as b:
   with self.assertRaisesRegex(RuntimeError,'missing tools'):build.all_steps()
   t.assert_not_called();b.assert_not_called()
 def test_failed_tests_stop_compile(self):
  with patch.object(build,'doctor'),patch.object(build,'test',side_effect=RuntimeError('failed test')),patch.object(build,'build') as b:
   with self.assertRaisesRegex(RuntimeError,'failed test'):build.all_steps()
   b.assert_not_called()
 @unittest.skipUnless(os.name=='nt','Windows file locking')
 def test_lock_rejects_concurrent_build_and_releases(self):
  with build.build_lock():
   with self.assertRaisesRegex(RuntimeError,'Another build'):
    with build.build_lock():pass
  with build.build_lock():pass
 def test_verify_uses_latest_receipt(self):
  self.art.mkdir();(self.art/'BUILD-RESULT.json').write_text('{"program":"new-run"}')
  with patch('verify_package.verify') as v:build.verify_latest();v.assert_called_once_with('new-run')
 def test_failed_command_records_status_and_output(self):
  with self.assertRaisesRegex(RuntimeError,r'failed \(7\)'):
   build.run('fixture',[sys.executable,'-c','import sys;print("literal stdout");print("literal stderr",file=sys.stderr);sys.exit(7)'])
  row=next((self.art/'logs').glob('*.json'));self.assertEqual(json.loads(row.read_text())['exit_code'],7)
  self.assertIn('literal stdout',row.with_suffix('.stdout.txt').read_text());self.assertIn('literal stderr',row.with_suffix('.stderr.txt').read_text())
 @unittest.skipUnless(os.name=='nt','Windows batch entry')
 def test_cmd_help_works_from_another_directory(self):
  env=os.environ.copy();env['SCBL_NO_PAUSE']='1'
  # Controlled developer PATH, not accumulated historical SDK paths on the host.
  env['PATH']=str(Path(sys.executable).parent)+';'+os.environ['SystemRoot']+'/System32'
  r=subprocess.run([str(Path(os.environ['SystemRoot'])/'System32/cmd.exe'),'/d','/c',str(REPO/'build.cmd'),'--help'],cwd=self.root,env=env,capture_output=True)
  self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertIn(b'--check',r.stdout)

class BundledInputTests(unittest.TestCase):
 def test_all_bundled_assets_match_manifest(self):
  rows=json.loads((REPO/'scripts/local-assets.json').read_text(encoding='utf-8-sig'))['files']
  self.assertEqual(len(rows),20)
  for name,row in rows.items():
   with self.subTest(name=name):
    data=(REPO/name).read_bytes();self.assertEqual(len(data),row['bytes']);self.assertEqual(hashlib.sha256(data).hexdigest(),row['sha256'])
 def test_save_template_files_match_manifest(self):
  root=REPO/'src/launcher/src/Assets/DefaultSaves';rows=json.loads((root/'manifest.json').read_text(encoding='utf-8-sig'))['Files']
  self.assertEqual(len(rows),9)
  for name,row in rows.items():
   with self.subTest(name=name):
    data=(root/name).read_bytes();self.assertEqual(len(data),row['bytes']);self.assertEqual(hashlib.sha256(data).hexdigest(),row['sha256'])
 def test_mod_sidecar_keeps_six_current_records_and_checksum(self):
  import struct
  data=(REPO/'src/launcher/src/Assets/DefaultSaves/MOD/00000001.scbl-loadouts').read_bytes()
  self.assertEqual(data[:8],b'SCBLLDO1');self.assertEqual(struct.unpack_from('<HH',data,8),(3,6));self.assertEqual(hashlib.sha256(data[:-32]).digest(),data[-32:])

if __name__=='__main__':unittest.main()
