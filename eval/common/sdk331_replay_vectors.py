"""Freeze real local embedding output per exact input for semantic replay only.

No generation and no synthetic vectors. Live performance runs bypass this adapter.
"""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import time
import httpx
from .sdk331_live import append, rows


class FrozenEmbeddings:
    def __init__(self, root, upstream='http://127.0.0.1:8081'):
        self.client = httpx.Client(timeout=120, trust_env=False)
        self.cache = {r['key']:r['embedding'] for r in rows(root/'frozen-vectors.jsonl')}
        self.lock = threading.Lock()
        owner=self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*_):pass
            def do_GET(self):
                response=owner.client.get(upstream+self.path)
                self.reply(response.status_code,response.content)
            def reply(self,status,body):
                self.send_response(status);self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            def do_POST(self):
                try:
                    body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    if self.path!='/v1/embeddings':
                        response=owner.client.post(upstream+self.path,json=body)
                        return self.reply(response.status_code,response.content)
                    inputs=body['input'];assert isinstance(inputs,list)
                    keys=[hashlib.sha256(json.dumps(x,ensure_ascii=False).encode()).hexdigest() for x in inputs]
                    with owner.lock:
                        missing={k:x for k,x in zip(keys,inputs) if k not in owner.cache}
                        if missing:
                            start=time.perf_counter()
                            response=owner.client.post(upstream+self.path,json=dict(body,input=list(missing.values())))
                            response.raise_for_status();data=response.json()['data']
                            data=sorted(data,key=lambda x:x['index']);assert len(data)==len(missing)
                            for (key,value),row in zip(missing.items(),data):
                                owner.cache[key]=row['embedding']
                                append(root/'frozen-vectors.jsonl',{'key':key,'input':value,'embedding':row['embedding']})
                            append(root/'embedding-upstream.jsonl',{'inputs':len(missing),'elapsedMs':(time.perf_counter()-start)*1000})
                        result={'object':'list','data':[{'object':'embedding','index':i,'embedding':owner.cache[k]} for i,k in enumerate(keys)]}
                    self.reply(200,json.dumps(result).encode())
                except Exception as exc:self.reply(500,json.dumps({'error':str(exc)}).encode())
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.url=f'http://127.0.0.1:{self.server.server_port}'
        threading.Thread(target=self.server.serve_forever,daemon=True).start()

    def close(self):
        self.server.shutdown();self.server.server_close();self.client.close()
