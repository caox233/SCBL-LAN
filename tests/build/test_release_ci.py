from pathlib import Path
import hashlib,importlib.util,json,os,sys,tempfile,unittest,zipfile
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[2];sys.path.insert(0,str(ROOT/'scripts'));import release_ci as ci

class ReleaseChecks(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
  p=self.root/'src/launcher/src';p.mkdir(parents=True);(p/'SplinterCellCNLauncher.csproj').write_text('<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup></Project>')
  self.identity={'release_id':'scbl-lan-1.0.0-test','compat_id':'scbl-lan-v1'};(self.root/'release-identity.json').write_text(json.dumps(self.identity))
  (self.root/'docs').mkdir();(self.root/'docs/RELEASE-NOTES.md').write_text('release notes')
  self.art=self.root/'artifacts';self.program=self.art/'program';self.program.mkdir(parents=True);(self.program/'SCBL.LAN.exe').write_bytes(b'fixture')
  self.archive=self.art/'fixture.zip'
  with zipfile.ZipFile(self.archive,'w') as z:z.write(self.program/'SCBL.LAN.exe','SCBL.LAN.exe')
  self.receipt={'status':'built-and-package-verified','release':self.identity,'archive':str(self.archive),'program':str(self.program),'archive_sha256':ci.sha(self.archive),'components':{}}
  self.ctx={'repository':'owner/repo','commit':'a'*40,'run_id':'123','run_url':'https://github.com/owner/repo/actions/runs/123'}
 def test_version_matches(self):self.assertEqual(ci.check_tag('v1.0.0',self.root),self.identity)
 def test_version_mismatch_rejected(self):
  with self.assertRaises(ValueError):ci.check_tag('v2.0.0',self.root)
 def test_tag_injection_rejected(self):
  for tag in ['v1.0.0;echo bad','../x','v1.0.0\n','v1.0.0-beta']:
   with self.subTest(tag=tag),self.assertRaises(ValueError):ci.check_tag(tag,self.root)
 def test_local_publication_rejected(self):
  with patch.dict(os.environ,{},clear=True),self.assertRaises(RuntimeError):ci.context()
 def test_invalid_context_rejected(self):
  with patch.dict(os.environ,{'GITHUB_ACTIONS':'true','GITHUB_REPOSITORY':'bad','GITHUB_SHA':'short','GITHUB_RUN_ID':'123'},clear=True),self.assertRaises(RuntimeError):ci.context()
 def test_stage_and_verify_exact_bytes(self):
  out=self.art/'release';ci.stage('v1.0.0',self.receipt,out,self.ctx,self.root)
  self.assertEqual((out/'SCBL-LAN-1.0.0-windows.zip').read_bytes(),self.archive.read_bytes());self.assertEqual(len(ci.verify_assets(out,'v1.0.0',self.ctx)),3)
 def test_failed_build_rejected(self):
  self.receipt['status']='failed'
  with self.assertRaises(ValueError):ci.stage('v1.0.0',self.receipt,self.art/'release',self.ctx,self.root)
 def test_hash_mismatch_rejected(self):
  self.receipt['archive_sha256']='0'*64
  with self.assertRaises(ValueError):ci.stage('v1.0.0',self.receipt,self.art/'release',self.ctx,self.root)
 def test_verified_directory_mismatch_rejected(self):
  (self.program/'SCBL.LAN.exe').write_bytes(b'different')
  with self.assertRaises(ValueError):ci.stage('v1.0.0',self.receipt,self.art/'release',self.ctx,self.root)
 def test_existing_output_preserved(self):
  out=self.art/'release';out.mkdir();(out/'keep').write_text('keep')
  with self.assertRaises(FileExistsError):ci.stage('v1.0.0',self.receipt,out,self.ctx,self.root)
  self.assertEqual((out/'keep').read_text(),'keep')
 def test_other_run_artifact_rejected(self):
  out=self.art/'release';ci.stage('v1.0.0',self.receipt,out,self.ctx,self.root)
  with self.assertRaises(ValueError):ci.verify_assets(out,'v1.0.0',dict(self.ctx,run_id='456'))
 def test_tampered_release_asset_rejected(self):
  out=self.art/'release';ci.stage('v1.0.0',self.receipt,out,self.ctx,self.root);(out/'SCBL-LAN-1.0.0-windows.zip').write_bytes(b'bad')
  with self.assertRaises(ValueError):ci.verify_assets(out,'v1.0.0',self.ctx)

if __name__=='__main__':unittest.main()
