"""Release catalog construction retained from the verified local packager."""
from pathlib import Path
import hashlib,json,os,re,stat

COMPONENTS = {
    'build-main': ('runtime/uplay_r1_loader.dll', 0x14c, True),
    'build-gameplay': ('runtime/scbl_gameplay.dll', 0x14c, True),
    'build-server': ('runtime/SCBL.LAN.Server.exe', 0x8664, False),
    'build-launcher': ('SCBL.LAN.exe', 0x14c, False),
}

RELEASE_ROLES = [('launcher','build-launcher'),('main','build-main'),
                 ('gameplay','build-gameplay'),('server','build-server')]

def build_release_catalog(program, identity_path=None):
    identity_path = identity_path or Path(__file__).resolve().parent/'work/release-identity.json'
    identity = read(identity_path)
    if set(identity) != {'release_id','compat_id'} or any(not isinstance(value,str) or not re.fullmatch(r'[A-Za-z0-9._-]{1,64}',value) for value in identity.values()):
        raise ValueError('invalid prebuild release identity')
    rows=[]
    for role,action in RELEASE_ROLES:
        relative=COMPONENTS[action][0]
        if relative not in program:raise ValueError(f'missing component role: {role}')
        path=safe_path(program[relative]);digest=hashlib.sha256();size=0;tail=b'';markers=0
        prefix=f'SCBL_RELEASE_V1|{role}|'
        expected=prefix+identity['release_id']+'|'+identity['compat_id']+'|END_SCBl'
        patterns=[(prefix.encode('ascii'),expected.encode('ascii')),
                  (prefix.encode('utf-16-le'),expected.encode('utf-16-le'))]
        with path.open('rb') as file:
            while True:
                block=file.read(65536);digest.update(block);size+=len(block);tail+=block
                scan=len(tail) if not block else max(0,len(tail)-512)
                for prefix,expected in patterns:
                    offset=tail.find(prefix)
                    while 0<=offset<scan:
                        if not tail[offset:].startswith(expected):raise ValueError(f'embedded identity marker mismatch: {role}')
                        markers+=1;offset=tail.find(prefix,offset+1)
                # A role file cannot also carry another complete role declaration.
                for other,_ in RELEASE_ROLES:
                    if other==role:continue
                    pattern=rb'SCBL_RELEASE_V1\|'+other.encode()+rb'\|[A-Za-z0-9._-]{1,64}\|[A-Za-z0-9._-]{1,64}\|END_SCBl'
                    for match in re.finditer(pattern,tail):
                        if match.start()<scan:raise ValueError(f'foreign role identity marker: {role}/{other}')
                    wide_prefix=('SCBL_RELEASE_V1|'+other+'|').encode('utf-16-le')
                    offset=tail.find(wide_prefix)
                    while 0<=offset<scan:
                        sample=tail[offset:offset+400]
                        sample=sample[:len(sample)//2*2].decode('utf-16-le',errors='ignore')
                        if re.match(pattern.decode('ascii'),sample):raise ValueError(f'foreign role identity marker: {role}/{other}')
                        offset=tail.find(wide_prefix,offset+1)
                if not block:break
                tail=tail[scan:]
        if size==0 or markers!=1:raise ValueError(f'missing or ambiguous embedded identity marker: {role}')
        rows.append({'role':role,'path':relative,**identity,'bytes':size,'sha256':digest.hexdigest()})
    return json_bytes({'schema':1,**identity,'components':rows})

def read(path):
    return json.loads(safe_path(path).read_text(encoding='utf-8-sig'))

def safe_path(path):
    path=Path(os.path.abspath(path))
    for p in [*reversed(path.parents),path]:
        try:
            s=p.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(s.st_mode) or getattr(s,'st_file_attributes',0)&0x400:
            raise ValueError(f'reparse input/output is forbidden: {p}')
    return path

def json_bytes(value):return (json.dumps(value,sort_keys=True,ensure_ascii=False,indent=2)+'\n').encode('utf-8')
