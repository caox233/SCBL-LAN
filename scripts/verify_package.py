"""Verify complete package integrity without running a game or accessing user data."""
from pathlib import Path,PurePosixPath
import hashlib,json,sys
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(root):
 root=Path(root).resolve();catalog=json.loads((root/'package/RELEASE-CATALOG.json').read_text(encoding='utf-8-sig'))
 assert catalog['schema']==1 and len(catalog['components'])==4,'catalog shape'
 assert {r['role'] for r in catalog['components']}=={'launcher','main','gameplay','server'},'catalog roles'
 for r in catalog['components']:
  rel=PurePosixPath(r['path']);assert not rel.is_absolute() and '..' not in rel.parts and ':' not in str(rel),'unsafe path'
  p=root/str(rel);assert p.is_file() and p.stat().st_size==r['bytes'] and sha(p)==r['sha256'],r['role']+' integrity'
  assert r['release_id']==catalog['release_id'] and r['compat_id']==catalog['compat_id'],'mixed release'
 manifest=json.loads((root/'package/PACKAGE-MANIFEST.json').read_text(encoding='utf-8-sig'))
 for n,r in manifest['files'].items():
  rel=PurePosixPath(n);assert not rel.is_absolute() and '..' not in rel.parts and ':' not in n
  p=root/n;assert p.stat().st_size==r['bytes'] and sha(p)==r['sha256'],n
 sums=(root/'package/SHA256SUMS.txt').read_text().splitlines()
 for row in sums:
  h,n=row.split('  ',1);rel=PurePosixPath(n);assert not rel.is_absolute() and '..' not in rel.parts and ':' not in n
  assert sha(root/n)==h,n
 print('PACKAGE_VERIFIED',catalog['release_id'],'roles=4','manifest_files='+str(len(manifest['files'])))
 return catalog
if __name__=='__main__':verify(Path(sys.argv[1]))
