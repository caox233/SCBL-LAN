"""GitHub-hosted release wrapper; the local build entry never publishes."""
from pathlib import Path,PurePosixPath
import argparse,hashlib,json,os,re,shutil,subprocess,sys,tempfile,xml.etree.ElementTree as ET,zipfile

ROOT=Path(__file__).resolve().parents[1]
ART=ROOT/'artifacts'

def read(path):return json.loads(path.read_text(encoding='utf-8-sig'))
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def write(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def check_tag(tag,root=ROOT):
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+',tag):raise ValueError('Use a plain version tag such as v1.0.0')
    version=ET.parse(root/'src/launcher/src/SplinterCellCNLauncher.csproj').findtext('.//Version')
    if tag!='v'+str(version):raise ValueError('Tag must match launcher Version: '+str(version))
    identity=read(root/'release-identity.json')
    if not identity['release_id'].startswith('scbl-lan-'+version+'-'):raise ValueError('release_id must match the launcher version')
    return identity

def context():
    if os.environ.get('GITHUB_ACTIONS')!='true':raise RuntimeError('Prepare/publish is restricted to GitHub Actions; local builds remain local')
    repo=os.environ.get('GITHUB_REPOSITORY','');commit=os.environ.get('GITHUB_SHA','');run=os.environ.get('GITHUB_RUN_ID','')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',repo) or not re.fullmatch(r'[a-f0-9]{40}',commit) or not run.isdigit():raise RuntimeError('Invalid GitHub execution context')
    return {'repository':repo,'commit':commit,'run_id':run,'run_url':f'https://github.com/{repo}/actions/runs/{run}'}

def package_checks(program):
    out=ART/'package-checks';out.mkdir(exist_ok=True)
    for flag,name in [('--lan-tools-self-test','lan-tools'),('--self-test','launcher')]:
        report=out/(name+'.txt')
        p=subprocess.run([str(program/'SCBL.LAN.exe'),flag,str(report)],cwd=program,capture_output=True,timeout=180)
        (out/(name+'.stdout.txt')).write_bytes(p.stdout);(out/(name+'.stderr.txt')).write_bytes(p.stderr)
        write(out/(name+'.json'),{'exit':p.returncode,'command':[flag,str(report)]})
        text=report.read_text(encoding='utf-8-sig') if report.exists() else ''
        if p.returncode!=0 or 'FAIL' in text or 'PASS' not in text:raise RuntimeError('Packaged check failed: '+name)
        print('PACKAGE_CHECK_OK',name,text.count('PASS'),flush=True)

def stage(tag,receipt,destination,ctx,root=ROOT):
    identity=check_tag(tag,root)
    if receipt['status']!='built-and-package-verified' or receipt['release']!=identity:raise ValueError('No matching successful build')
    archive=Path(receipt['archive']);program=Path(receipt['program'])
    for p in [archive,program]:
        if not p.resolve().is_relative_to((root/'artifacts').resolve()):raise ValueError('Build output outside artifacts')
    if sha(archive)!=receipt['archive_sha256']:raise ValueError('Build ZIP hash mismatch')
    with zipfile.ZipFile(archive) as z:
        if z.testzip() is not None:raise ValueError('ZIP corruption')
        names=z.namelist()
        if len(names)!=len(set(names)):raise ValueError('Duplicate ZIP member')
        for name in names:
            rel=PurePosixPath(name)
            if rel.is_absolute() or '..' in rel.parts or ':' in name or '\\' in name or name.startswith('data/'):raise ValueError('Unsafe ZIP member')
            if z.read(name)!=(program/name).read_bytes():raise ValueError('ZIP and verified directory differ')
    destination.mkdir(parents=True,exist_ok=False)
    name='SCBL-LAN-'+tag[1:]+'-windows.zip';shutil.copyfile(archive,destination/name)
    provenance={'schema':1,**ctx,'tag':tag,'release':identity,'archive':name,'archive_sha256':sha(destination/name),'components':receipt['components'],'built_on':'GitHub-hosted Windows runner','game_runtime_tested':False}
    write(destination/'BUILD-PROVENANCE.json',provenance)
    (destination/'SHA256SUMS.txt').write_text(''.join(sha(destination/n)+'  '+n+'\n' for n in [name,'BUILD-PROVENANCE.json']),encoding='ascii')
    notes=(root/'docs/RELEASE-NOTES.md').read_text(encoding='utf-8')
    notes+='\n\n### 构建来源\n\n'+f'- GitHub Actions：[查看云端构建]({ctx["run_url"]})\n- 源码提交：`{ctx["commit"]}`\n- 完整包身份：`{identity["release_id"]}`\n- 下载 `'+name+'`，不是 GitHub 自动生成的 Source code 压缩包。\n'
    (destination/'RELEASE-NOTES.md').write_text(notes,encoding='utf-8')
    return provenance

def verify_assets(folder,tag,ctx):
    p=read(folder/'BUILD-PROVENANCE.json')
    if any(p.get(k)!=ctx[k] for k in ('repository','commit','run_id')) or p.get('tag')!=tag:raise ValueError('Artifact provenance differs from this workflow run')
    name='SCBL-LAN-'+tag[1:]+'-windows.zip'
    if p['archive']!=name or sha(folder/name)!=p['archive_sha256']:raise ValueError('Archive differs from provenance')
    expected={name,'BUILD-PROVENANCE.json'};actual=set()
    for line in (folder/'SHA256SUMS.txt').read_text(encoding='ascii').splitlines():
        digest,n=line.split('  ',1)
        if n not in expected or n in actual or sha(folder/n)!=digest:raise ValueError('Invalid checksum list')
        actual.add(n)
    if actual!=expected:raise ValueError('Incomplete checksum list')
    return [name,'BUILD-PROVENANCE.json','SHA256SUMS.txt']

def gh(*args):
    p=subprocess.run(['gh',*args],capture_output=True,text=True,encoding='utf-8')
    if p.returncode:raise RuntimeError('gh '+args[0]+' failed: '+p.stderr)
    return p.stdout

def publish(tag):
    ctx=context();check_tag(tag);folder=ART/'release';names=verify_assets(folder,tag,ctx);repo=ctx['repository']
    releases=json.loads(gh('api',f'repos/{repo}/releases?per_page=100'))
    existing=next((r for r in releases if r['tag_name']==tag),None)
    if existing:
        if not existing['draft'] or ctx['run_url'] not in (existing.get('body') or '') or existing['target_commitish']!=ctx['commit']:raise RuntimeError('Release already exists; never overwrite a published or unrelated release')
    else:
        gh('release','create',tag,'--repo',repo,'--draft','--target',ctx['commit'],'--title','SCBL LAN '+tag[1:],'--notes-file',str(folder/'RELEASE-NOTES.md'))
    info=json.loads(gh('release','view',tag,'--repo',repo,'--json','assets,isDraft'))
    current={a['name']:a for a in info['assets']}
    for name in names:
        if name not in current:gh('release','upload',tag,str(folder/name),'--repo',repo)
    # Verify the actual uploaded bytes while the release is still a draft.
    with tempfile.TemporaryDirectory() as tmp:
        gh('release','download',tag,'--repo',repo,'--dir',tmp)
        for name in names:
            if sha(Path(tmp)/name)!=sha(folder/name):raise RuntimeError('Uploaded asset hash mismatch: '+name)
    gh('release','edit',tag,'--repo',repo,'--draft=false','--prerelease=false','--latest')
    info=json.loads(gh('release','view',tag,'--repo',repo,'--json','url,isDraft,isPrerelease,tagName,assets'))
    if info['isDraft'] or info['isPrerelease'] or info['tagName']!=tag:raise RuntimeError('Release was not published')
    print('RELEASE_PUBLISHED',info['url'],flush=True)

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('action',choices=['check','prepare','publish']);parser.add_argument('--tag',required=True);args=parser.parse_args()
    check_tag(args.tag)
    if args.action=='check':print('RELEASE_CONFIG_OK',args.tag);return
    if args.action=='publish':publish(args.tag);return
    ctx=context();receipt=read(ART/'BUILD-RESULT.json');program=Path(receipt['program'])
    from verify_package import verify
    verify(program);package_checks(program)
    p=stage(args.tag,receipt,ART/'release',ctx);print('RELEASE_ASSETS_READY',p['archive'],flush=True)

if __name__=='__main__':
    try:main()
    except Exception as exc:print('ERROR',str(exc),file=sys.stderr);sys.exit(1)
