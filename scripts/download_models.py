#!/usr/bin/env python3
"""Resume pinned public weights in parallel, checking HTTP ranges and the upstream LFS SHA256."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import time
import urllib.request

def sha256(path):
    h=hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda:f.read(1<<20),b''): h.update(chunk)
    return h.hexdigest()

def download(name,meta,workers,chunk_mib):
    filename='pytorch_model.bin' if name=='bge-m3' else 'model.safetensors'
    detail=meta['fileMetadata'][filename]
    size=detail['size'];expected=detail['lfs']['sha256']
    target=Path('models')/name/filename
    target.parent.mkdir(parents=True,exist_ok=True)
    prefix=target.stat().st_size if target.exists() else 0
    if prefix==size and sha256(target)==expected:
        return {'model':name,'bytes':size,'sha256':expected,'verified':True}
    if prefix>=size: raise RuntimeError(f'{name}: existing weight has invalid size/hash')
    parts=target.with_name(target.name+'.parts');parts.mkdir(exist_ok=True)
    ranges=[(start,min(start+chunk_mib*(1<<20),size)-1) for start in range(prefix,size,chunk_mib*(1<<20))]
    print(f'{name}: keeping {prefix/(1<<20):.1f} MiB prefix, {len(ranges)} chunks, {workers} connections',flush=True)
    base='https://huggingface.co/'+meta['repo']+'/resolve/'+meta['revision']+'/'+filename
    def fetch(bounds):
        start,end=bounds;path=parts/f'{start}-{end}'
        for attempt in range(6):
            got=path.stat().st_size if path.exists() else 0
            if got==end-start+1: return path
            if got>end-start+1: raise RuntimeError('oversized range cache')
            request=urllib.request.Request(base+f'?download=true&part={start+got}-{end}',
                headers={'Range':f'bytes={start+got}-{end}'})
            try:
                with urllib.request.urlopen(request,timeout=90) as response:
                    want=f'bytes {start+got}-{end}/{size}'
                    if response.status!=206 or response.headers.get('Content-Range')!=want:
                        raise RuntimeError(f'{name}: server did not honor requested range')
                    with path.open('ab') as f:
                        while True:
                            block=response.read(1<<18)
                            if not block: break
                            f.write(block)
                if path.stat().st_size!=end-start+1: raise RuntimeError('truncated range')
                return path
            except Exception as e:
                if attempt==5: raise
                print(f'{name}: retry range {start}, {type(e).__name__}',flush=True)
                time.sleep(min(30,2**attempt))
    paths={}
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        future={pool.submit(fetch,r):r for r in ranges}
        for f in concurrent.futures.as_completed(future):
            start,_=future[f];paths[start]=f.result()
            print(f'{name}: chunks {len(paths)}/{len(ranges)}',flush=True)
    assembled=target.with_name(target.name+'.assembling')
    with assembled.open('wb') as out:
        if prefix:
            with target.open('rb') as old: shutil.copyfileobj(old,out,1<<20)
        for start in sorted(paths):
            with paths[start].open('rb') as part: shutil.copyfileobj(part,out,1<<20)
    actual=sha256(assembled)
    if assembled.stat().st_size!=size or actual!=expected:
        raise RuntimeError(f'{name}: upstream SHA256 verification failed; original prefix retained')
    os.replace(assembled,target)
    print(f'{name}: complete and SHA256 verified',flush=True)
    return {'model':name,'bytes':size,'sha256':actual,'verified':True}

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--workers-per-model',type=int,default=8)
    parser.add_argument('--chunk-mib',type=int,default=32);parser.add_argument('--manifest',type=Path,default=Path(__file__).resolve().parent.parent/'examples/model-manifest.json');args=parser.parse_args()
    if args.workers_per_model<1 or args.chunk_mib<1: parser.error('positive limits required')
    manifest=json.loads(args.manifest.read_text())
    for name,meta in manifest.items():
        target=Path('models')/name;target.mkdir(parents=True,exist_ok=True)
        for filename,expected in meta['assets'].items():
            f=target/filename
            if f.exists():
                if sha256(f)!=expected: raise RuntimeError('Existing model asset checksum differs: '+str(f))
                continue
            url='https://huggingface.co/'+meta['repo']+'/resolve/'+meta['revision']+'/'+filename
            data=urllib.request.urlopen(url,timeout=90).read()
            if hashlib.sha256(data).hexdigest()!=expected: raise RuntimeError('Model asset checksum failed: '+filename)
            f.write_bytes(data)
    Path('results').mkdir(exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        futures=[pool.submit(download,name,meta,args.workers_per_model,args.chunk_mib) for name,meta in manifest.items()]
        results=[f.result() for f in futures]
    Path('results/model-download.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results))

if __name__=='__main__': main()
