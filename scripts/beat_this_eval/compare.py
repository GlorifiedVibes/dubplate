"""Compares Beat This! (run.py) with Dubplate's grid: tempo, grid offset, which beat of our bar its downbeats fall on, and
whether the labelled drops (samples/bar_labels.txt) sit on its downbeats.

    cargo run --release -p dubplate-engine --example gridjson -- FILE... > $BT_DIR/ours.tsv
    BT_DIR=beat_this_work python scripts/beat_this_eval/compare.py small0-v2.onnx final0-v2.onnx
"""
import json,sys,numpy as np
import os
D=os.environ.get('BT_DIR','beat_this_work')+'/'
ours={}
for l in open(D+'ours.tsv'):
    p=l.rstrip('\n').split('\t'); ours[p[0]]=dict(bpm=float(p[1]),phase=float(p[2]),anchor=float(p[3]),res=int(p[4]),conf=float(p[5]))
labels={}
for l in open(os.environ.get('BT_LABELS','samples/bar_labels.txt')):
    if l.startswith('#') or not l.strip(): continue
    p=l.rstrip('\n').split('\t'); labels[p[0]]=[float(x) for x in p[1:] if x.strip()]
def mode(a): 
    v,c=np.unique(a,return_counts=True); return v[np.argmax(c)],c.max()/len(a)
tot=dict(agree=0,n=0,dr_ours=0,dr_bt=0,dr=0)
for model in sys.argv[1:]:
    bt=json.load(open(D+'out_'+model+'.json')); print('=====',model)
    print('%-34s %7s %7s | %8s | bar1 vs ours (beats, share) | drops on BT downbeat'%('track','ourBPM','btBPM','gridoff ms'))
    for f,o in ours.items():
        if f not in bt: continue
        b=np.array(bt[f]['beats']); d=np.array(bt[f]['downbeats'])
        ibi=np.median(np.diff(b)); bbpm=60/ibi
        per=60/o['bpm']
        # compare beat times to our grid: median offset of BT beats from nearest grid beat
        k=np.round((b-o['phase'])/per); off=(b-(o['phase']+k*per))*1000
        mid=(b>20)&(b<bt[f]['secs']-20)
        go=np.median(off[mid]) if mid.sum()>10 else float('nan')
        # which beat of our grid do BT downbeats sit on: (index of BT downbeat - our anchor index) mod 4
        kd=np.round((d-o['anchor'])/per); m,share=mode(kd.astype(int)%4)
        drs=''
        for t in labels.get(f,[]):
            near=d[np.argmin(abs(d-t))]; drs+=' %.1f:%s'%(t,'BTyes' if abs(near-t)<0.07 else 'BTno(%+.0fms)'%((t-near)*1000))
            tot['dr']+=1; tot['dr_bt']+=abs(near-t)<0.07
        tot['n']+=1; tot['agree']+= (m==0)
        print('%-34s %7.2f %7.2f | %+8.1f | %d (%.0f%%) conf %.1f%s'%(f.split('/')[-1][:34],o['bpm'],bbpm,go,m,100*share,o['conf'],drs))
print(tot)
