"""Local build/test/package entry. Never installs, launches the game, or publishes."""
from pathlib import Path,PurePosixPath
from contextlib import contextmanager
import argparse,hashlib,json,os,shutil,subprocess,sys,datetime,zipfile,time
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
 args=list(map(str,args))
 # On Windows CreateProcess does not use the replacement env PATH to find exe.
 if not Path(args[0]).is_absolute():
  resolved=shutil.which(args[0],path=(env or os.environ).get('PATH'))
  if resolved:args[0]=resolved
 folder=ART/'logs';folder.mkdir(parents=True,exist_ok=True)
 stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
 base=folder/(stamp+'-'+label)
 print('RUN',label,' '.join(map(str,args)),flush=True)
 with base.with_suffix('.stdout.txt').open('wb') as out,base.with_suffix('.stderr.txt').open('wb') as err:
  try:p=subprocess.Popen(list(map(str,args)),cwd=cwd or ROOT,env=env,stdout=out,stderr=err)
  except OSError as e:
   err.write((str(e)+'\n').encode('utf-8'))
   write(base.with_suffix('.json'),{'command':list(map(str,args)),'cwd':str(cwd or ROOT),'exit_code':None,'launch_error':str(e)})
   raise RuntimeError(f'{label} could not start; logs: {base}') from e
  started=time.monotonic()
  try:
   while True:
    try:p.wait(timeout=15);break
    except subprocess.TimeoutExpired:print('WORKING',label,f'{int(time.monotonic()-started)}s; logs: {base}',flush=True)
  except KeyboardInterrupt:
   p.terminate();p.wait();raise
 write(base.with_suffix('.json'),{'command':list(map(str,args)),'cwd':str(cwd or ROOT),'exit_code':p.returncode})
 print('EXIT',label,p.returncode,flush=True)
 if p.returncode:
  for suffix in ('.stdout.txt','.stderr.txt'):
   print(base.with_suffix(suffix).read_text(encoding='utf-8',errors='replace')[-6000:])
  raise RuntimeError(f'{label} failed ({p.returncode}); logs: {base}')
 return base
def dotnet():
 p=CONFIG.get('dotnet') or shutil.which('dotnet')
 if not p:raise RuntimeError('Install .NET SDK 10.0.302, or set dotnet in build.local.json')
 return p
def native_env():
 env=os.environ.copy();tc=CONFIG.get('toolchain_root')
 original_path=env.get('PATH',env.get('Path',''))
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
 path=env.pop('Path',env.get('PATH',''))
 cmake=Path(vs)/'Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin'
 if not (cmake/'cmake.exe').exists():
  found=subprocess.check_output([str(vswhere),'-latest','-products','*','-requires','Microsoft.VisualStudio.Component.VC.CMake.Project','-find',r'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'],env=clean,text=True).strip().splitlines()
  if found:cmake=Path(found[0]).parent
 env['PATH']=merge_paths(str(Path(cargo).parent),str(cmake),path,original_path)
 env['CARGO_TARGET_DIR']=str(Path(CONFIG.get('cargo_target_dir',ART/'cargo')).resolve())
 env['CARGO_INCREMENTAL']='0'
 # Match the accepted R2 build policy. Do not inherit private diagnostic build modes.
 for k in ('SCBL_VISION_CAUSAL_BUILD','SCBL_VISION_COMPARE_BUILD','SCBL_WEAPON_DIAGNOSTICS'):env.pop(k,None)
 env['SCBL_EMF_CONSTANT_REUSE_BUILD']='1';env['SCBL_THERMAL_MATERIAL_OBSERVER_BUILD']='1'
 return cargo,env
def merge_paths(*values):
 seen=set();parts=[]
 for value in values:
  for part in value.split(';'):
   key=part.rstrip('\\/').casefold()
   if key and key not in seen:seen.add(key);parts.append(part)
 return ';'.join(parts)

def asset_source(explicit=None):
 if explicit is not None:return Path(explicit)
 if CONFIG.get('asset_dir'):return Path(CONFIG['asset_dir']).expanduser().resolve()
 return next((p for p in (ROOT/'build-assets.zip',ROOT/'build-assets') if p.exists()),None)

def assets(source=None):
 rows=read(ROOT/'scripts/local-assets.json')['files'];missing=[]
 pending={};source=asset_source(source)
 archive=zipfile.ZipFile(source) if source and source.is_file() else None
 try:
  for name,row in rows.items():
   rel=PurePosixPath(name)
   if rel.is_absolute() or '..' in rel.parts or ':' in name or '\\' in name:raise RuntimeError('Unsafe asset manifest path: '+name)
   p=ROOT/name
   if p.exists():
    if not p.is_file() or digest(p)!=row['sha256']:missing.append(name+' (existing file differs; not overwritten)')
    continue
   data=None
   if archive:
    entries=[x for x in archive.infolist() if x.filename==name]
    if len(entries)==1 and entries[0].file_size==row['bytes']:data=archive.read(entries[0])
   elif source and (source/name).is_file():data=(source/name).read_bytes()
   if data is None or len(data)!=row['bytes'] or hashlib.sha256(data).hexdigest()!=row['sha256']:missing.append(name)
   else:pending[name]=data
 finally:
  if archive:archive.close()
 if missing:raise RuntimeError('Required local assets missing/mismatched:\n'+'\n'.join(missing)+'\nProvide authorized build-assets.zip in the repository root, or use --asset-dir <ZIP-or-directory>. See docs/BUILD.md. No files were imported.')
 # Validate every input first; never extract unlisted ZIP entries.
 for name,data in pending.items():
  p=ROOT/name;p.parent.mkdir(parents=True,exist_ok=True)
  with p.open('xb') as f:f.write(data)
 print('ASSETS_OK',len(rows),'imported='+str(len(pending)))
def doctor():
 errors=[]
 if os.name!='nt':raise RuntimeError('Windows x64 is required for the WPF launcher and native DLLs.')
 try:assets()
 except Exception as e:errors.append(str(e))
 try:run('dotnet-version',[dotnet(),'--version'])
 except Exception as e:errors.append('.NET SDK: '+str(e))
 try:
  cargo,env=native_env()
  for label,args in [('rust-version',[cargo,'--version']),('git-version',['git','--version']),('cmake-version',['cmake','--version']),('protoc-version',[env.get('PROTOC','protoc'),'--version'])]:
   try:run(label,args,ROOT/'src/native',env)
   except Exception as e:errors.append(label+': '+str(e))
 except Exception as e:errors.append('Native toolchain: '+str(e))
 if errors:raise RuntimeError('BUILD REQUIREMENTS NOT READY:\n\n'+'\n\n'.join(errors)+'\n\nSetup instructions: '+str(ROOT/'docs/BUILD.md'))
 print('DOCTOR_OK independent project configuration')

@contextmanager
def build_lock():
 # The OS releases this lock on exit/crash; never remove a competing lock.
 import msvcrt
 ART.mkdir(parents=True,exist_ok=True)
 with (ART/'.build.lock').open('a+b') as f:
  f.seek(0,os.SEEK_END)
  if f.tell()==0:f.write(b'0');f.flush()
  f.seek(0)
  try:msvcrt.locking(f.fileno(),msvcrt.LK_NBLCK,1)
  except OSError:raise RuntimeError('Another build/test is running in this checkout; wait for it to finish.') from None
  try:yield
  finally:f.seek(0);msvcrt.locking(f.fileno(),msvcrt.LK_UNLCK,1)

def all_steps():
 for name,action in [('1/4 Check requirements',doctor),('2/4 Regression tests',test),('3/4 Build and package',build),('4/4 Verify complete package',verify_latest)]:
  print('\n=== '+name+' ===',flush=True);action()
 print('BUILD_SUCCESS',read(ART/'BUILD-RESULT.json')['archive'],flush=True)

def verify_latest(package=None):
 from verify_package import verify
 if package is None:
  receipt=ART/'BUILD-RESULT.json'
  package=read(receipt)['program'] if receipt.exists() else ART/'program'/read(ROOT/'release-identity.json')['release_id']
 verify(package)

def output_paths(release_id):
 if not release_id or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-' for c in release_id) or release_id in {'.','..'}:raise RuntimeError('Unsafe release_id')
 name=release_id
 while True:
  out=ART/'program'/name;archive=ART/(name+'-program.zip')
  if not out.exists() and not archive.exists():return out,archive
  name=release_id+'-build-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
def test():
 run('test-build-entry',[sys.executable,'-B','-m','unittest','discover','-s',ROOT/'tests/build','-v'])
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
 identity=read(ROOT/'release-identity.json');out,archive=output_paths(identity['release_id'])
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
 with zipfile.ZipFile(archive,'x',zipfile.ZIP_DEFLATED) as z:
  for p in sorted(out.rglob('*')):
   if p.is_file():z.write(p,p.relative_to(out).as_posix())
 with zipfile.ZipFile(archive) as z:assert z.testzip() is None
 write(ART/'BUILD-RESULT.json',{'status':'built-and-package-verified','release':identity,'program':str(out),'archive':str(archive),'archive_sha256':digest(archive),'components':{k:{'path':names[k],'sha256':digest(out/names[k])} for k in names},'game_tested':False,'published':False})
 print('PACKAGE_OK',out,archive,flush=True)
def main():
 p=argparse.ArgumentParser(description=__doc__)
 p.add_argument('action',choices=['all','doctor','assets','test','build','verify'],nargs='?',default='all')
 p.add_argument('--asset-dir',type=Path,help='Authorized asset ZIP or directory; never downloaded automatically')
 p.add_argument('--package',type=Path);p.add_argument('--check',action='store_true',help='Check requirements only; do not compile');a=p.parse_args()
 if sys.version_info<(3,11):raise RuntimeError('Python 3.11+ is required')
 if a.asset_dir:CONFIG['asset_dir']=str(a.asset_dir.resolve())
 if a.check:a.action='doctor'
 if a.action=='verify':verify_latest(a.package)
 else:
  with build_lock():
   {'all':all_steps,'doctor':doctor,'assets':assets,'test':test,'build':build}[a.action]()
if __name__=='__main__':
 try:main()
 except Exception as e:print('ERROR',str(e),file=sys.stderr);sys.exit(1)
