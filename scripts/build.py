"""Local build/test/package entry. Never installs, launches the game, or publishes."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,subprocess,sys,datetime,zipfile
ROOT=Path(__file__).resolve().parents[1]
ART=ROOT/'artifacts'
CONFIG=json.loads((ROOT/'build.local.json').read_text(encoding='utf-8-sig')) if (ROOT/'build.local.json').exists() else {}
def digest(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  for b in iter(lambda:f.read(1048576),b''):h.update(b)
 return h.hexdigest()
def read(p):return json.loads(p.read_text(encoding='utf-8-sig'))
def write(p,x):p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps(x,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def run(label,args,cwd=None,env=None):
 folder=ART/'logs';folder.mkdir(parents=True,exist_ok=True)
 stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
 base=folder/(stamp+'-'+label)
 print('RUN',label,' '.join(map(str,args)),flush=True)
 with base.with_suffix('.stdout.txt').open('wb') as out,base.with_suffix('.stderr.txt').open('wb') as err:
  p=subprocess.run(list(map(str,args)),cwd=cwd or ROOT,env=env,stdout=out,stderr=err)
 write(base.with_suffix('.json'),{'command':list(map(str,args)),'cwd':str(cwd or ROOT),'exit_code':p.returncode})
 print('EXIT',label,p.returncode,flush=True)
 if p.returncode:
  print(base.with_suffix('.stderr.txt').read_text(encoding='utf-8',errors='replace')[-6000:])
  raise RuntimeError(f'{label} failed ({p.returncode}); logs: {base}')
 return base
def dotnet():
 p=CONFIG.get('dotnet') or shutil.which('dotnet')
 if not p:raise RuntimeError('Install .NET SDK 10.0.302, or set dotnet in build.local.json')
 return p
def native_env():
 env=os.environ.copy();tc=CONFIG.get('toolchain_root')
 if tc:
  tc=Path(tc);env['CARGO_HOME']=str(tc/'cargo');env['RUSTUP_HOME']=str(tc/'rustup');env['PROTOC']=str(tc/'protoc-35.1/bin/protoc.exe')
 cargo=str(Path(env.get('CARGO_HOME',Path.home()/'.cargo'))/'bin/cargo.exe')
 if not Path(cargo).exists():cargo=shutil.which('cargo')
 if not cargo:raise RuntimeError('Install Rust/rustup; see docs/BUILD.md')
 vswhere=Path(os.environ.get('ProgramFiles(x86)','C:/Program Files (x86)'))/'Microsoft Visual Studio/Installer/vswhere.exe'
 if not vswhere.exists():raise RuntimeError('Visual Studio C++ Build Tools (x86/x64 and CMake) required')
 vs=subprocess.check_output([str(vswhere),'-latest','-products','*','-requires','Microsoft.VisualStudio.Component.VC.Tools.x86.x64','-property','installationPath'],text=True).strip()
 vc=Path(vs)/'Common7/Tools/VsDevCmd.bat'
 if not vc.exists():raise RuntimeError('Visual Studio C++ toolchain not found')
 clean=env.copy();windows=Path(os.environ['SystemRoot']);clean['PATH']=';'.join(map(str,[windows/'System32',windows,windows/'System32/Wbem']))
 raw=subprocess.check_output(f'cmd.exe /d /s /c ""{vc}" -no_logo -arch=x64 -host_arch=x64 && set"',env=clean,text=True)
 for line in raw.splitlines():
  if '=' in line and not line.startswith('='):
   k,v=line.split('=',1);env[k]=v
 path=env.pop('Path',env.get('PATH',''));env['PATH']=str(Path(cargo).parent)+';'+path
 env['CARGO_TARGET_DIR']=str(Path(CONFIG.get('cargo_target_dir',ART/'cargo')).resolve())
 env['CARGO_INCREMENTAL']='0'
 # Match the accepted R2 build policy. Do not inherit private diagnostic build modes.
 for k in ('SCBL_VISION_CAUSAL_BUILD','SCBL_VISION_COMPARE_BUILD','SCBL_WEAPON_DIAGNOSTICS'):env.pop(k,None)
 env['SCBL_EMF_CONSTANT_REUSE_BUILD']='1';env['SCBL_THERMAL_MATERIAL_OBSERVER_BUILD']='1'
 return cargo,env
def assets(source=None):
 rows=read(ROOT/'scripts/local-assets.json')['files'];missing=[]
 for name,row in rows.items():
  p=ROOT/name
  if source and not p.exists():
   q=Path(source)/name
   if q.is_file() and digest(q)==row['sha256']:p.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(q,p)
  if not p.is_file() or digest(p)!=row['sha256']:missing.append(name)
 if missing:raise RuntimeError('Required local assets missing/mismatched (not downloaded automatically):\n'+'\n'.join(missing))
 print('ASSETS_OK',len(rows))
def doctor():
 assets();run('dotnet-version',[dotnet(),'--version']);cargo,env=native_env();run('rust-version',[cargo,'--version'],ROOT/'src/native',env)
 run('protoc-version',[env.get('PROTOC','protoc'),'--version'],env=env)
 print('DOCTOR_OK independent project configuration')
def test():
 # Existing tests call actual production classes; no historical source-text extraction.
 for name in ('PortPlan','BackgroundTaskRegression'):
  run('test-'+name,[dotnet(),'run','--project',ROOT/f'tests/{name}/{name}.csproj','-c','Release','--no-launch-profile'])
 cargo,env=native_env();cmd=[cargo,'test','--locked','-p','hooks-plugin-api','-p','hooks-config','--lib']
 if CONFIG.get('offline'):cmd.append('--offline')
 run('test-native-core',cmd,ROOT/'src/native',env)
 print('CORE_TESTS_OK; full historical/runtime validation remains local')
def build():
 assets();cargo,env=native_env();native=ROOT/'src/native'
 for label,args in [('main',['-p','hooks','--lib','--features','patch-free,lan-goal-origin']),('gameplay',['-p','scbl-gameplay','--lib','--features','goggle-feedback']),('server',['-p','dedicated_server','--bin','dedicated_server'])]:
  cmd=[cargo,'build','--release','--locked',*args]
  if CONFIG.get('offline'):cmd.append('--offline')
  run('build-'+label,cmd,native,env)
 target=Path(env['CARGO_TARGET_DIR'])
 components={'main':target/'i686-pc-windows-msvc/release/hooks.dll','gameplay':target/'i686-pc-windows-msvc/release/scbl_gameplay.dll','server':target/'release/dedicated_server.exe'}
 # Generated binding is a build artifact, never a mutation of checked-in templates.
 manifest=read(ROOT/'src/launcher/src/Assets/DefaultSaves/manifest.json');manifest['ModRuntimeSha256']=digest(components['gameplay'])
 generated=ART/'generated/default-saves-manifest.json';write(generated,manifest)
 publish=ART/'launcher';obj=ART/'obj/launcher'
 run('publish-launcher',[dotnet(),'publish',ROOT/'src/launcher/src/SplinterCellCNLauncher.csproj','-c','Release','-r','win-x86','--self-contained','true','-o',publish,'--nologo',f'-p:BaseIntermediateOutputPath={obj}{os.sep}',f'-p:ScblDefaultSaveManifest={generated}','-p:NuGetAudit=false'])
 components['launcher']=publish/'SCBL.LAN.exe'
 package(components)
def package(components):
 from package_catalog import build_release_catalog
 from verify_package import verify
 identity=read(ROOT/'release-identity.json');out=ART/'program'/identity['release_id']
 if out.exists():raise RuntimeError(f'Output already exists; preserve it or choose a new release_id: {out}')
 out.mkdir(parents=True)
 names={'launcher':'SCBL.LAN.exe','main':'runtime/uplay_r1_loader.dll','gameplay':'runtime/scbl_gameplay.dll','server':'runtime/SCBL.LAN.Server.exe'}
 program={}
 for role,name in names.items():
  p=out/name;p.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(components[role],p);program[name]=p
  if role!='launcher':p.with_name(p.name+'.sha256').write_text(digest(p)+'\n',encoding='ascii')
 (out/'package').mkdir();(out/'licenses').mkdir()
 shutil.copyfile(ROOT/'LICENSE',out/'licenses/LICENSE')
 shutil.copyfile(ROOT/'THIRD_PARTY_NOTICES.txt',out/'licenses/THIRD_PARTY_NOTICES.txt')
 for p in (ROOT/'src/launcher/src/ThirdPartyNotices').glob('*.txt'):shutil.copyfile(p,out/'licenses'/p.name)
 shutil.copyfile(ROOT/'docs/PACKAGE-README.txt',out/'README.txt')
 (out/'package/RELEASE-CATALOG.json').write_bytes(build_release_catalog(program,ROOT/'release-identity.json'))
 rows={p.relative_to(out).as_posix():{'sha256':digest(p),'bytes':p.stat().st_size} for p in out.rglob('*') if p.is_file()}
 write(out/'package/PACKAGE-MANIFEST.json',{'schema':1,'kind':'local-repository-validation',**identity,'files':rows})
 sums=''.join(digest(p)+'  '+p.relative_to(out).as_posix()+'\n' for p in sorted(out.rglob('*')) if p.is_file())
 (out/'package/SHA256SUMS.txt').write_text(sums,encoding='utf-8')
 verify(out)
 archive=ART/(identity['release_id']+'-program.zip')
 with zipfile.ZipFile(archive,'x',zipfile.ZIP_DEFLATED) as z:
  for p in sorted(out.rglob('*')):
   if p.is_file():z.write(p,p.relative_to(out).as_posix())
 with zipfile.ZipFile(archive) as z:assert z.testzip() is None
 write(ART/'BUILD-RESULT.json',{'status':'built-and-package-verified','release':identity,'program':str(out),'archive':str(archive),'archive_sha256':digest(archive),'components':{k:{'path':names[k],'sha256':digest(out/names[k])} for k in names},'game_tested':False,'published':False})
 print('PACKAGE_OK',out,archive,flush=True)
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['doctor','assets','test','build','verify']);p.add_argument('--asset-dir',type=Path);p.add_argument('--package',type=Path);a=p.parse_args()
 if a.action=='assets':assets(a.asset_dir)
 elif a.action=='doctor':doctor()
 elif a.action=='test':test()
 elif a.action=='build':build()
 else:
  from verify_package import verify
  verify(a.package or ART/'program'/read(ROOT/'release-identity.json')['release_id'])
if __name__=='__main__':
 try:main()
 except Exception as e:print('ERROR',str(e),file=sys.stderr);sys.exit(1)
