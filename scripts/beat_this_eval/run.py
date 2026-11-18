"""Runs Beat This! (https://github.com/CPJKU/beat_this, MIT) over a list of files and saves its beats and downbeats.

    BT_DIR=beat_this_work python scripts/beat_this_eval/run.py small0-v2.onnx FILES.txt

BT_DIR holds the ONNX model (from https://huggingface.co/ashudesai/songbird-models) and mel-filterbank.bin (from
https://huggingface.co/musetric/beat-this-onnx); neither is in the repository. FILES.txt has one audio path a line.
Needs numpy, scipy, soxr, onnxruntime and ffmpeg. The pre- and post-processing follow beat_this/preprocessing.py,
inference.py and postprocessor.py ("minimal", no DBN).
"""
import numpy as np, subprocess, sys, soxr, onnxruntime as ort, json
import os
D=os.environ.get('BT_DIR','beat_this_work')+'/'
fb=np.fromfile(D+'mel-filterbank.bin',dtype=np.float32).reshape(513,128)
model=sys.argv[1]; sess=ort.InferenceSession(D+model,providers=['CPUExecutionProvider'])
def load(f):
    raw=subprocess.run(['ffmpeg','-v','quiet','-i',f,'-f','f32le','-ac','2','-ar','44100','-'],capture_output=True).stdout
    x=np.frombuffer(raw,np.float32).reshape(-1,2).mean(1)
    return soxr.resample(x,44100,22050).astype(np.float32)
def spect(x):
    n_fft,hop=1024,441
    p=np.pad(x,(n_fft//2,n_fft//2),mode='reflect')
    nfr=1+(len(p)-n_fft)//hop
    idx=np.arange(n_fft)[None,:]+hop*np.arange(nfr)[:,None]
    w=np.hanning(n_fft+1)[:-1].astype(np.float32)  # periodic hann
    S=np.abs(np.fft.rfft(p[idx]*w,axis=1))/32.0
    return np.log1p(1000*(S.astype(np.float32)@fb)).astype(np.float32)
def run(sp,chunk=1500,border=6):
    T=len(sp); out_b=np.full(T,-1000.,np.float32); out_d=out_b.copy()
    starts=np.arange(-border,T-border,chunk-2*border)
    if T>chunk-2*border: starts[-1]=T-(chunk-border)
    res=[]
    for s in starts:
        a,b=max(s,0),min(s+chunk,T); c=sp[a:b]
        c=np.pad(c,((max(0,-s),max(0,min(border,s+chunk-T))),(0,0)))
        o=sess.run(None,{'spectrogram':c[None]})
        res.append((s,o[0][0],o[1][0]))
    for s,b,d in reversed(res):
        lo=s+border; hi=s+chunk-border
        # predictions index i corresponds to frame s+i (with left pad counted as in upstream: pad shifts)
        seg=slice(border,len(b)-border)
        fr0=s+border; 
        L=min(len(b)-2*border, T-fr0)
        if L>0 and fr0>=0:
            out_b[fr0:fr0+L]=b[border:border+L]; out_d[fr0:fr0+L]=d[border:border+L]
    return out_b,out_d
def peaks(l):
    from scipy.ndimage import maximum_filter1d
    m=maximum_filter1d(l,7,mode='nearest'); pk=(l==m)&(l>0)
    f=np.where(pk)[0]; 
    if len(f)==0: return f.astype(float)
    g=[[f[0]]]
    for v in f[1:]:
        (g[-1].append(v) if v-g[-1][-1]<=1 else g.append([v]))
    return np.array([np.mean(x) for x in g])/50.0
if __name__=='__main__':
    import time
    out={}
    for line in open(sys.argv[2]):
        f=line.rstrip('\n')
        if not f: continue
        t=time.time(); x=load(f); 
        try: b,d=run(spect(x))
        except Exception as e: print('fail',f,e); continue
        bt=peaks(b); dt=peaks(d)
        if len(bt)==0: continue
        dt=np.unique(bt[[np.argmin(abs(bt-v)) for v in dt]])
        out[f]={'beats':bt.tolist(),'downbeats':dt.tolist(),'secs':len(x)/22050,'took':time.time()-t}
        print(f.split('/')[-1],len(bt),len(dt),'%.1fs for %.0fs audio'%(time.time()-t,len(x)/22050),flush=True)
    json.dump(out,open(D+'out_'+model+'.json','w'))
