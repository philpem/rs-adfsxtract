#!/usr/bin/env python3
"""Build a single-zone E-format (new-map) FileCore/ADFS disc image from a nested
file tree, per philpem/arcology filecore_guide.md appendix C.
Two-pass: Phase 1 allocates every object (files + directories) as a fragment and
records its offset; Phase 2 writes directory blocks that reference children by
SIN.  Validated with the authoritative readers (acornfsextract/filecore-extract).

Geometry (800 KB E-format floppy):
  sector_size=1024, spt=5, heads=2, density=2, idlen=15, bpmb=128 (log2bpmb=7),
  nzones=1, zone_spare=1312, disc_size=819200, root_dir SIN=0x203 (frag2/off3).
Allocation unit = 128 bytes.  Fragment descriptor = [idlen-bit id][zeros][1];
width = allocation units, never shorter than idlen+1=16 units (2048 bytes).
"""
import math, struct

SECTOR=1024; BPMB=128; IDLEN=15; MINUNITS=16; U=8
DISC_SIZE=819200; ROOT_ADDR=0x800; SYSTEM_UNITS=0x1000//BPMB  # 32
DISK_NAME=b'Cmpnion2_5'
ATTR_DIRECTORY=0x08
FID_SYSTEM=2

def disc_record_bytes():
    b=bytearray(32)
    b[0]=10; b[1]=5; b[2]=2; b[3]=2; b[4]=15; b[5]=7; b[6]=1; b[7]=0
    b[8]=0x00; b[9]=1
    b[10:12]=struct.pack('<H',1312)
    struct.pack_into('<I',b,0x0c,(2<<8)|3)
    struct.pack_into('<I',b,0x10,DISC_SIZE)
    nm=DISK_NAME[:10]; b[0x16:0x16+len(nm)]=nm
    return bytes(b)

def put_bits(buf, bitpos, bits):
    for i,bb in enumerate(bits):
        if bb:
            p=bitpos+i; buf[p>>3] |= (1<<(p&7))

def encode_frag(bits, fid, units):
    for i in range(IDLEN): bits.append((fid>>i)&1)
    bits += [0]*(units-IDLEN-1)
    bits.append(1)

def zone_check_xor(sec):
    # guide A.1 (match the reference reader exactly): genuine 32-bit ADCS
    # add-with-carry chain over words in reverse, keeping a 32-bit running
    # sum and a separate carry, then drop final carry, subtract byte0,
    # fold 32->8 via two sequential XOR-shifts.
    words=[struct.unpack_from('<I',sec,i)[0] for i in range(0,len(sec)-len(sec)%4,4)]
    s=0; carry=0
    for w in reversed(words):
        s1=s+w
        c1 = 1 if s1 > 0xffffffff else 0
        s1 &= 0xffffffff
        s2=s1+carry
        c2 = 1 if s2 > 0xffffffff else 0
        s=s2 & 0xffffffff
        carry = 1 if (c1 or c2) else 0
    s = (s - sec[0]) & 0xffffffff
    s = s ^ (s>>16)
    s = s ^ (s>>8)
    return s & 0xff

def rotl(v,n,w=32):
    n%=w; return ((v<<n)|(v>>(w-n)))&((1<<w)-1)
def ror13(v): 
    # rotate right by 13 within 32 bits
    return ((v>>13)|(v<<(32-13)))&0xffffffff

def dir_check_byte(buf, end_of_entries, dir_end):
    # guide A.2 / variant B — verified reproduces real media
    checksum = 0
    # Region 1: whole 32-bit words from 0 up to word-aligned <= end_of_entries,
    # then leftover non-word bytes up to end_of_entries.
    pos = 0
    while pos + 4 <= end_of_entries:
        checksum = struct.unpack_from('<I', buf, pos)[0] ^ ror13(checksum)
        pos += 4
    while pos < end_of_entries:
        checksum = buf[pos] ^ ror13(checksum)
        pos += 1
    # Skip the tail end-marker byte at 0x7d7 entirely; resume at 0x7d8.
    pos = 0x7d8
    # Region 2: whole words then leftover bytes, stopping BEFORE the final word
    # (dir_end - 4), which contains the check byte itself.
    while pos + 4 <= dir_end - 4:
        checksum = struct.unpack_from('<I', buf, pos)[0] ^ ror13(checksum)
        pos += 4
    while pos < dir_end - 4:
        checksum = buf[pos] ^ ror13(checksum)
        pos += 1
    # two-step fold (guide A.2) - do not add a third >>24 step
    checksum = checksum ^ (checksum >> 16)
    checksum = checksum ^ (checksum >> 8)
    return checksum & 0xff

class DirNode:
    def __init__(self, name): self.name=name; self.children=[]  # list of DirNode or FileNode
class FileNode:
    def __init__(self, name, data, load, exec_, attrs):
        self.name=name; self.data=data; self.load=load; self.exec=exec_; self.attrs=attrs

class Builder:
    def __init__(self):
        self.data=bytearray(DISC_SIZE)
        self.disc_size=DISC_SIZE
        self.next_unit=SYSTEM_UNITS
        self.next_fid=FID_SYSTEM
        self.objects=[]   # (kind, node, fid, units, offset)
    def _units_for(self, length):
        need=max(math.ceil(length/BPMB), MINUNITS)
        return ((need+U-1)//U)*U
    def _new_fid(self):
        self.next_fid+=1; return self.next_fid
    def alloc(self, kind, node, length):
        fid=self._new_fid()
        units=self._units_for(length)
        off=self.next_unit*BPMB
        self.next_unit+=units
        self.objects.append((kind,node,fid,units,off))
        return fid, off

    def build(self, root_dirnode):
        root_dirnode._off = ROOT_ADDR
        root_dirnode._sin = (FID_SYSTEM<<8) | 3   # 0x203: fragment 2, sharing offset 3
        # phase 1: allocate all non-root objects (pre-order)
        for kind, child in root_dirnode.children:
            self._alloc_recursive(child, kind=='dir')
        # phase 2: write directory blocks + file data
        self._write_recursive(root_dirnode)
        self._write_maps()
        return bytes(self.data)

    def _alloc_recursive(self, node, is_dir):
        if is_dir:
            fid, off = self.alloc('dir', node, 0x800)
            node._fid=fid; node._off=off; node._sin=(fid<<8)|0x00
            for kind, c in node.children:
                self._alloc_recursive(c, kind=='dir')
        else:
            fid, off = self.alloc('file', node, len(node.data))
            node._fid=fid; node._off=off; node._sin=(fid<<8)|0x00

    def _write_recursive(self, node):
        # build entries referencing children
        entries=[]
        for kind, child in node.children:
            if kind=='dir':
                entries.append({'name':child.name,'load':0xfffffd45,'exec':0,
                                'length':0x800,'sin':child._sin,'attrs':ATTR_DIRECTORY})
            else:
                entries.append({'name':child.name,'load':child.load,'exec':child.exec,
                                'length':len(child.data),'sin':child._sin,'attrs':child.attrs})
        addr = node._off   # always set (root=ROOT_ADDR, others allocated)
        self._write_dirblock(addr, entries, name=node.name.encode('latin1') if node.name else b'$',
                             parent=addr)
        for kind, child in node.children:
            if kind=='dir': self._write_recursive(child)
        # write file data
        for kind, child in node.children:
            if kind=='file':
                self.data[child._off:child._off+len(child.data)]=child.data

    def _write_dirblock(self, addr, entries, name, parent):
        buf=bytearray(0x800)
        buf[0]=0; buf[1:5]=b'Nick'
        for i,e in enumerate(entries):
            if i>=77: break
            rec=bytearray(26)
            nm=e['name'][:10].encode('latin1'); rec[0:len(nm)]=nm
            if len(nm)<10: rec[len(nm)]=0x0d
            struct.pack_into('<I',rec,0x0a,e['load']&0xffffffff)
            struct.pack_into('<I',rec,0x0e,e['exec']&0xffffffff)
            struct.pack_into('<I',rec,0x12,e['length'])
            sin=e['sin']; rec[0x16]=sin&0xff; rec[0x17]=(sin>>8)&0xff; rec[0x18]=(sin>>16)&0xff
            rec[0x19]=e['attrs']&0xff
            buf[5+i*26:5+i*26+26]=rec
        buf[0x7d7]=0x00
        buf[0x7dd:0x7dd+19]=b'\x00'*19
        nm=name[:10]; buf[0x7f0:0x7f0+10]=nm+b'\x00'*(10-len(nm))
        buf[0x7fa]=0; buf[0x7fb:0x7ff]=b'Nick'
        p=parent; buf[0x7da]=p&0xff; buf[0x7db]=(p>>8)&0xff; buf[0x7dc]=(p>>16)&0xff
        end_of_entries=5+26*len(entries)
        buf[0x7ff]=dir_check_byte(buf, end_of_entries, 0x800)
        self.data[addr:addr+0x800]=buf

    def _write_maps(self):
        bits=[]
        encode_frag(bits, FID_SYSTEM, SYSTEM_UNITS)
        for (kind,node,fid,units,off) in self.objects:
            encode_frag(bits, fid, units)
        free_units=max(self.disc_size//BPMB - self.next_unit, MINUNITS)
        free_bitpos=len(bits)
        encode_frag(bits, 0, free_units)
        mapsec=bytearray(SECTOR)
        dr=disc_record_bytes()
        mapsec[0x04:0x04+len(dr)]=dr
        put_bits(mapsec, 64*8, bits)
        freelink=(64*8)+free_bitpos-8
        freelink|=0x8000
        struct.pack_into('<H',mapsec,0x01,freelink&0xffff)
        mapsec[0x03]=0xff
        # Find the byte 0 that makes the reader's own zone_check self-consistent
        # (the reader includes byte 0's real value in the ADCS sum, so a naive
        # computation with byte0=0 does not round-trip).
        for candidate in range(256):
            mapsec[0x00]=candidate
            if zone_check_xor(mapsec)==candidate:
                break
        self.data[0x000:SECTOR]=mapsec
        self.data[0x400:0x400+SECTOR]=mapsec
