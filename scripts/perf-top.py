#!/usr/bin/env python3
"""Aggregate an xctrace time-profile export: python3 perf-top.py tp.xml [min_seconds] [n_inclusive]

Produce tp.xml with:
  xctrace export --input run.trace --output tp.xml \
    --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]'
"""
import sys, collections
import xml.etree.ElementTree as ET

path = sys.argv[1]
t_min = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
ids = {}

def res(e):
    if e is None: return None
    r = e.get("ref")
    if r is not None: return ids[r]
    if e.get("id"): ids[e.get("id")] = e
    return e

self_c, incl_c, threads = collections.Counter(), collections.Counter(), collections.Counter()
total = 0
for _, row in ET.iterparse(path, events=("end",)):
    if row.tag != "row": continue
    st = res(row.find("sample-time")); th = res(row.find("thread")); bt = res(row.find("tagged-backtrace"))
    for c in row.iter():  # register ids of every nested element
        if c.get("id"): ids[c.get("id")] = c
    if st is None or bt is None: continue
    if int(st.text) / 1e9 < t_min: continue
    frames = []
    for f in bt.iter("frame"):
        f = res(f)
        frames.append(f.get("name"))
    if not frames: continue
    threads[th.get("fmt") if th is not None else "?"] += 1
    total += 1
    self_c[frames[0]] += 1
    for n in set(frames): incl_c[n] += 1
    row.clear()

print(f"samples: {total} (>= {t_min}s)")
print("\nthreads:");  [print(f"  {c*100/total:5.1f}%  {n}") for n, c in threads.most_common(6)]
print("\nself:");     [print(f"  {c*100/total:5.1f}%  {n[:120]}") for n, c in self_c.most_common(20)]
print("\ninclusive:");[print(f"  {c*100/total:5.1f}%  {n[:120]}") for n, c in incl_c.most_common(int(sys.argv[3]) if len(sys.argv) > 3 else 45)]
