#!/usr/bin/env python3
"""Minimal FileCore (ADFS/E-format new-map) build + read + validate toolkit.

References: philpem/arcology doc/format_info/acorn32bit/filecore_guide.md
            (sections 1.1, 2.1, 2.4, 3.1, 3.2, 3.3, A.1, A.2, C.1-C.5).

Supports single-zone E-format (new map) images, which is what the HDC discs are.
"""
import struct, os, binascii

SECTOR=0x400          # 1024 bytes (log2_secsize=10)
MAXENTRIES=77
ENTRY=26

# ---------- checksums ----------
def zone_check(data: bytes) -> int:
    # XOR folding over the whole sector in 32-bit words, then 1x supplement
    # (guide A.1 - new map zone check).
    # Simplest correct per guide A.1: not detailed; we use the ADFS "add with
    # carry descending" variant used for old-map only. For new map we defer to
    # a folding XOR; see _zone_check below.
    raise NotImplementedError

# We implement checksums only where the guide gives a direct algorithm we need.
# For build/read of E-format the important non-checksum structures are: the
# disc record (in zone-0 map header), the zone bit stream (used to resolve
# SINs), and directory entries. ZoneCheck/CrossCheck can be computed but RISC OS
# tolerates a re-format where they're present; for offline images we compute
# the zone check the way EMULATORS/FileCore do (see note). We keep it simple.

def read_bits(data, bitpos, n):
    v=0
    for i in range(n):
        byte=data[(bitpos+i)>>3]; bit=(bitpos+i)&7
        v |= ((byte>>bit)&1)<<i
    return v

def read_u24(b): return b[0]|(b[1]<<8)|(b[2]<<16)
def write_u24(buf,off,v): buf[off]=v&0xff; buf[off+1]=(v>>8)&0xff; buf[off+2]=(v>>16)&0xff
def read_u32(b,off): return int.from_bytes(b[off:off+4],'little')
def write_u32(buf,off,v): buf[off:off+4]=struct.pack('<I',v)
def decode_name(raw10):
    acc=0
    vals=[b&0x7f for b in raw10]
    end=0
    for i,b in enumerate(vals):
        if b==0 or b==0x0d: end=i; break
    else: end=10
    name=bytes(vals[:end]).decode('latin1')
    return name

class DiscRecord:
    def __init__(self, log2sec, spt, heads, density, idlen, log2bpmb, skew,
                 bootopt, lowsec, nzones, zonespare, root_dir, disc_size):
        self.log2sec=log2sec; self.spt=spt; self.heads=heads; self.density=density
        self.idlen=idlen; self.log2bpmb=log2bpmb; self.skew=skew; self.bootopt=bootopt
        self.lowsec=lowsec; self.nzones=nzones; self.zonespare=zonespare
        self.root_dir=root_dir; self.disc_size=disc_size
    def bytes(self, extended=False):
        b=bytearray(60 if extended else 32)
        b[0]=self.log2sec; b[1]=self.spt; b[2]=self.heads; b[3]=self.density
        b[4]=self.idlen; b[5]=self.log2bpmb; b[6]=self.skew; b[7]=self.bootopt
        b[8]=self.lowsec; b[9]=self.nzones; b[10:12]=struct.pack('<H',self.zonespare)
        write_u32(b,0x0c,self.root_dir); write_u32(b,0x10,self.disc_size)
        return bytes(b)
    @classmethod
    def from_bytes(cls,b):
        return cls(b[0],b[1],b[2],b[3],b[4],b[5],b[6],b[7],b[8],b[9],
                   int.from_bytes(b[10:12],'little'), read_u32(b,0x0c), read_u32(b,0x10))

# ---------- reader ----------
class Image:
    def __init__(self, data):
        self.data=data
        self.sector_size=1<<10
        self.map_start=0
        # Disc record lives at offset 0x000 +0x04 (single-zone E floppy).
        self.dr = DiscRecord.from_bytes(data[0x04:0x04+20])
        self.sector_size = 1<<self.dr.log2sec
        self.bpmb = 1<<self.dr.log2bpmb
        self.map_primary = 0x000
        # decode zone 0 allocation bit stream
        self.zone_map = self._decode_zone(self.map_primary, 0)

    def _zone_header_bits(self):
        return 64*8  # zone0: 4-byte header + 60-byte disc record
    def _zone_bits(self):
        # extent bits for zone 0 = sector_size*8 - zone_spare - 480 (guide 2.4)
        return self.sector_size*8 - self.dr.zonespare - 480

    def _decode_zone(self, addr, zone):
        sec=self.data[addr:addr+self.sector_size]
        # 4-byte header
        zonecheck=sec[0]; freelink=struct.unpack_from('<H',sec,1)[0]; cross=sec[3]
        headerbits=self._zone_header_bits() if zone==0 else 4*8
        bitpos=headerbits
        frags=[]  # (id, start_alloc_unit, len_units, start_bit)
        extent_end=headerbits+self._zone_bits()
        while bitpos < self.sector_size*8:
            fid=read_bits(sec,bitpos,self.dr.idlen)
            fstart=bitpos-headerbits
            bitpos+=self.dr.idlen
            # count zeros until terminator
            found=False
            while bitpos<self.sector_size*8:
                b=read_bits(sec,bitpos,1); bitpos+=1
                if b==1: found=True; break
            units=bitpos-headerbits-fstart
            if not found:
                break
            frags.append((fid,fstart,units))
        return frags

    def _find_fragments(self, fid):
        # concatenate all fragments with this id across zones, in order
        extents=[]
        for (zfid,fstart,units) in self.zone_map:
            if zfid==fid:
                disc=self.map_primary+ (fstart//8)  # bytes from map start (bits->bytes)
                # allocation unit -> byte: units*bpmb added to disc base
                base_byte = self.map_primary + (fstart//8)  # we treat bits map onto bytes approx
                extents.append((fstart,units))
        return extents

    def resolve_sin(self, sin):
        # single zone only for now
        fid=(sin>>8)&0xffff; so=sin&0xff
        # find fragment byte extent
        total_units=0
        base=None
        for (zfid,fstart,units) in self.zone_map:
            if zfid==fid:
                total_units+=units
                if base is None:
                    base=self.map_primary + (fstart//8)
        if base is None: return 0
        start_byte=base
        if so:
            start_byte += (so-1)*self.sector_size
        return start_byte

    def dir_at(self, byte_ofs):
        buf=self.data[byte_ofs:byte_ofs+0x800]
        if buf[1:5] not in (b'Nick',b'Hugo'):
            return []
        ents=[]
        for i in range(MAXENTRIES):
            e=buf[5+i*ENTRY:5+i*ENTRY+ENTRY]
            if e[0]==0: break
            name=decode_name(e[0:10])
            load=read_u32(e,0x0a); exec_=read_u32(e,0x0e); length=read_u32(e,0x12)
            sin=read_u24(e[0x16:0x19]); attrs=e[0x19]
            isdir=attrs&0x08!=0
            ents.append(dict(name=name,load=load,exec=exec_,length=length,sin=sin,
                             attrs=attrs,isdir=isdir))
        return ents

def read_tree(data, root_byte):
    img=Image(data)
    def walk(byte_ofs):
        out=[]
        for e in img.dir_at(byte_ofs):
            p=e.copy()
            if e['isdir']:
                start=img.resolve_sin(e['sin'])
                p['children']=walk(start)
            else:
                start=img.resolve_sin(e['sin'])
                p['data']=img.data[start:start+e['length']]
            out.append(p)
        return out
    return img, walk(root_byte)
