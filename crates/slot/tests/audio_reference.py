#!/usr/bin/env python3
"""Build the supplied C references and generate f32le fixtures, without Cargo dependencies.
Run: python3 crates/slot/tests/audio_reference.py [--golden]
References must already exist in target/ref/ra-audio. Generated headers are ABI stubs,
not replacement signal processing. Golden fixtures retain the first 4096 input frames;
the live equivalence run uses all 8192 frames, switches DRC at frame 4096,
and switches to 6x fast-forward and back, in bulk and one-frame calls.
The small-call variant warms transitions in a batch before one-frame calls.
Per-part fixtures let Rust check the settled kernels separately from the C
crossfade, whose call-local pairing is intentionally corrected in Rust.
"""
# Copyright  (C) 2010-2020 The RetroArch team
#
# ---------------------------------------------------------------------------------------
# Port of libretro-common filters.h and fft.h helpers used by the reference harness.
# ---------------------------------------------------------------------------------------
#
# Permission is hereby granted, free of charge,
# to any person obtaining a copy of this software and associated documentation files (the "Software"),
# to deal in the Software without restriction, including without limitation the rights to
# use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software,
# and to permit persons to whom the Software is furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
# INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
# IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
# WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
#

from pathlib import Path
import math
import struct
import subprocess
import sys
import shutil

ROOT = Path(__file__).resolve().parents[3]
REF = ROOT / 'target/ref/ra-audio'
WORK = ROOT / 'target/audio-reference'
if sys.byteorder != 'little':
    raise SystemExit('Reference fixtures require a little-endian host')
WORK.mkdir(parents=True, exist_ok=True)
(WORK / 'audio').mkdir(exist_ok=True)
(WORK / 'fft').mkdir(exist_ok=True)
headers = {
'retro_inline.h': '#define INLINE inline\n',
'retro_posix_source.h': '',
'retro_math.h': '',
'retro_environment.h': '',
'retro_miscellaneous.h': '#define MIN(a,b) ((a)<(b)?(a):(b))\n',
'retro_atomic.h': '''typedef int retro_atomic_int_t;
static inline void retro_atomic_int_init(int *p,int v){*p=v;}
static inline int retro_atomic_fetch_sub_int(int *p,int v){int old=*p;*p-=v;return old;}
static inline void retro_atomic_inc_int(int *p){++*p;}
''',
'memalign.h': '''#include <stdlib.h>
static inline void *memalign_alloc(size_t a,size_t n){void *p=0;return posix_memalign(&p,a,n)?0:p;}
#define memalign_free free
''',
'filters.h': '''#include <math.h>
static inline double sinc(double x){return fabs(x)<0.00001?1.0:sin(x)/x;}
static inline double besseli0(double x){double sum=0,f=1,p=1,t=1;for(int i=0;i<18;i++){sum+=p*t/(f*f);p*=x*x;t*=0.25;f*=i+1;}return sum;}
static inline double kaiser_window_function(double x,double b){return besseli0(b*sqrtf(1-x*x));}
''',
'fft/fft.h': '''#ifndef FFT_STUB_H
#define FFT_STUB_H
#include <math.h>
typedef struct { float real,imag; } fft_complex_t;
typedef struct fft fft_t;
static inline fft_complex_t fft_complex_mul(fft_complex_t a,fft_complex_t b){return (fft_complex_t){a.real*b.real-a.imag*b.imag,a.real*b.imag+a.imag*b.real};}
static inline fft_complex_t fft_complex_add(fft_complex_t a,fft_complex_t b){return (fft_complex_t){a.real+b.real,a.imag+b.imag};}
static inline fft_complex_t fft_complex_sub(fft_complex_t a,fft_complex_t b){return (fft_complex_t){a.real-b.real,a.imag-b.imag};}
fft_t *fft_new(unsigned); void fft_free(fft_t*);
void fft_process_forward(fft_t*,fft_complex_t*,const float*,unsigned);
void fft_process_inverse(fft_t*,float*,const fft_complex_t*,unsigned);
#endif
''',
'libretro_dspfilter.h': '''#ifndef DSP_STUB_H
#define DSP_STUB_H
#include <stdint.h>
#define DSPFILTER_API_VERSION 1
typedef unsigned dspfilter_simd_mask_t;
struct dspfilter_info {float input_rate;};
struct dspfilter_input {float *samples;unsigned frames;};
struct dspfilter_output {float *samples;unsigned frames;};
struct dspfilter_input_i16 {int16_t *samples;unsigned frames;};
struct dspfilter_output_i16 {int16_t *samples;unsigned frames;};
struct dspfilter_config {
 int (*get_float)(void*,const char*,float*,float);
 int (*get_int)(void*,const char*,int*,int);
 int (*get_float_array)(void*,const char*,float**,unsigned*,const float*,unsigned);
 int (*get_int_array)(void*,const char*,int**,unsigned*,const int*,unsigned);
 int (*get_string)(void*,const char*,char**,const char*);
 void (*free)(void*);
};
struct dspfilter_implementation {
 void *(*init)(const struct dspfilter_info*,const struct dspfilter_config*,void*);
 void (*process)(void*,struct dspfilter_output*,const struct dspfilter_input*);
 void (*free)(void*);unsigned api_version;const char *ident,*short_ident;
 void (*process_i16)(void*,struct dspfilter_output_i16*,const struct dspfilter_input_i16*);
};
#endif
''',
'audio/audio_resampler.h': '''#ifndef RESAMPLER_STUB_H
#define RESAMPLER_STUB_H
#include <stddef.h>
#define RESAMPLER_API_VERSION 1
#define RESAMPLER_CAP_QUALITY 1
#define RESAMPLER_CAP_HQ_OVERSAMPLE 2
#define RESAMPLER_SIMD_SSE 1
#define RESAMPLER_SIMD_AVX 2
#define RESAMPLER_SIMD_NEON 4
typedef unsigned resampler_simd_mask_t;
enum resampler_quality {RESAMPLER_QUALITY_DONTCARE,RESAMPLER_QUALITY_LOWEST,RESAMPLER_QUALITY_LOWER,RESAMPLER_QUALITY_NORMAL,RESAMPLER_QUALITY_HIGHER,RESAMPLER_QUALITY_HIGHEST};
struct resampler_config {int unused;};
struct resampler_data {const float *data_in;float *data_out;size_t input_frames,output_frames;double ratio;};
typedef struct {
 void *(*init)(const struct resampler_config*,double,enum resampler_quality,resampler_simd_mask_t);
 void (*process)(void*,struct resampler_data*);void (*free)(void*);
 unsigned api_version;const char *ident,*short_ident;void (*reset)(void*);unsigned caps;void *(*sibling)(void*);
} retro_resampler_t;
#endif
''',
'audio/sinc_resampler.h': '',
'sinc_resampler_internal.h': '''#define SINC_HQ_CUTOFF 0.90
#define SINC_HQ_SIDELOBES 32
#define SINC_HQ_PHASE_BITS 10
#define SINC_HQ_SUBPHASE_BITS 14
#define SINC_HQ_KAISER_BETA 10.5
static inline int sinc_resampler_ratio_valid(double ratio,unsigned p,unsigned s){double step=(1u<<(p+s))/ratio;return isfinite(ratio)&&ratio>0&&step>=1&&step<=UINT32_MAX-(1u<<(p+s));}
''',
}
for name, text in headers.items():
    (WORK / name).write_text(text)
shutil.copyfile(REF / 'fft.c', WORK / 'fft/fft.c')
(WORK / 'harness.c').write_text(r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "libretro_dspfilter.h"
#include "audio/audio_resampler.h"
extern const struct dspfilter_implementation *eq_dspfilter_get_implementation(unsigned);
extern const struct dspfilter_implementation *reverb_dspfilter_get_implementation(unsigned);
extern const struct dspfilter_implementation *iir_dspfilter_get_implementation(unsigned);
extern const struct dspfilter_implementation *panning_dspfilter_get_implementation(unsigned);
extern retro_resampler_t sinc_resampler;
static int get_float(void*u,const char*k,float*v,float d){
 *v=d;if(!strcmp(k,"drytime"))*v=0.5f;else if(!strcmp(k,"wettime"))*v=0.15f;
 else if(!strcmp(k,"damping"))*v=0.8f;else if(!strcmp(k,"roomwidth")||!strcmp(k,"roomsize"))*v=0.25f;return 1;
}
static int get_int(void*u,const char*k,int*v,int d){*v=d;return 0;}
static int get_array(void*u,const char*k,float**v,unsigned*n,const float*d,unsigned dn){
 static const float freq[]={32,64,125,250,500,1000,2000,4000,8000,16000,20000};
 static const float gain[]={6,9,12,7,6,5,7,9,11,6,0};
 static const float left[]={0.75f,0},right[]={0,0.75f};
 if(!strcmp(k,"frequencies")){d=freq;dn=11;}else if(!strcmp(k,"gains")){d=gain;dn=11;}
 else if(!strcmp(k,"left_mix")){d=left;dn=2;}else if(!strcmp(k,"right_mix")){d=right;dn=2;}
 *n=dn;*v=malloc(dn*sizeof(float));memcpy(*v,d,dn*sizeof(float));return 1;
}
static int get_string(void*u,const char*k,char**v,const char*d){*v=strdup(!strcmp(k,"type")?"RIAA_CD":d);return 1;}
static const struct dspfilter_config cfg={get_float,get_int,get_array,NULL,get_string,free};
static void save(const char*prefix,const char*name,const float*x,size_t frames){char path[1024];snprintf(path,sizeof(path),"%s/%s.f32le",prefix,name);FILE*f=fopen(path,"wb");if(!f)exit(2);fwrite(x,2*sizeof(float),frames,f);fclose(f);}
int main(int argc,char**argv){
 if(argc!=4)return 2;const char*prefix=argv[2];size_t n=strtoul(argv[3],0,10);
 float *input=malloc(n*2*sizeof(float)),*copy=malloc(n*2*sizeof(float));
 FILE*f=fopen(argv[1],"rb");if(!f||fread(input,2*sizeof(float),n,f)!=n)return 2;fclose(f);
 save(prefix,"input",input,n);
 const struct dspfilter_implementation *impl[]={eq_dspfilter_get_implementation(0),reverb_dspfilter_get_implementation(0),iir_dspfilter_get_implementation(0),panning_dspfilter_get_implementation(0)};
 const char*names[]={"eq","reverb","iir","panning"};struct dspfilter_info info={32768};
 for(int i=0;i<4;i++){memcpy(copy,input,n*2*sizeof(float));void*state=impl[i]->init(&info,&cfg,NULL);if(!state)return 3;
 struct dspfilter_input in={copy,(unsigned)n};struct dspfilter_output out={0};impl[i]->process(state,&out,&in);save(prefix,names[i],out.samples,out.frames);impl[i]->free(state);}
 memcpy(copy,input,n*2*sizeof(float));struct dspfilter_input in={copy,(unsigned)n};
 void*states[4];for(int i=0;i<4;i++){states[i]=impl[i]->init(&info,&cfg,NULL);if(!states[i])return 3;struct dspfilter_output out={0};impl[i]->process(states[i],&out,&in);in.samples=out.samples;in.frames=out.frames;}
 save(prefix,"chain",in.samples,in.frames);for(int i=0;i<4;i++)impl[i]->free(states[i]);
 for(int change=0;change<2;change++){float*out=malloc((n*2+32)*2*sizeof(float));void*state=sinc_resampler.init(NULL,48000.0/32768,RESAMPLER_QUALITY_HIGHER,0);if(!state)return 3;
 size_t at=0;for(int half=0;half<2;half++){struct resampler_data d={input+half*n,out+at*2,n/2,0,48000.0/32768*(change&&half?1.004:1.0)};sinc_resampler.process(state,&d);at+=d.output_frames;}
 save(prefix,change?"sinc_change":"sinc",out,at);sinc_resampler.free(state);free(out);}
 for(int single=0;single<2;single++){
 float *out=malloc((n*2+32)*2*sizeof(float));void*state=sinc_resampler.init(NULL,48000.0/32768,RESAMPLER_QUALITY_HIGHER,0);if(!state)return 3;
 size_t at=0;for(int part=0;part<4;part++){
 size_t start=at;size_t block=single?1:n/4;
 for(size_t frame=0;frame<n/4;){size_t count=single&&frame==0&&(part==1||part==3)?1800:block;if(count>n/4-frame)count=n/4-frame;struct resampler_data d={input+part*n/2+frame*2,out+at*2,count,0,48000.0/32768*((part==1||part==2)?1.0/6.0:1.0)};sinc_resampler.process(state,&d);at+=d.output_frames;frame+=count;}
 if(n==8192){char name[80];snprintf(name,sizeof(name),"sinc_fast_forward%s_part%d",single?"_single":"",part);save(prefix,name,out+start*2,at-start);}}
 if(n==8192)save(prefix,single?"sinc_fast_forward_single":"sinc_fast_forward",out,at);sinc_resampler.free(state);free(out);}
 free(input);free(copy);return 0;
}
''')
cmd = ['cc', '-std=c99', '-D_POSIX_C_SOURCE=200809L', '-D_DEFAULT_SOURCE', '-DHAVE_FILTERS_BUILTIN', '-O2', '-ffp-contract=off', '-I'+str(WORK), str(WORK/'harness.c')]
cmd += [str(REF / (name+'.c')) for name in ('eq','reverb','iir','panning','sinc_resampler')]
cmd += ['-lm','-o',str(WORK/'harness')]
print(' '.join(cmd), flush=True)
subprocess.run(cmd, check=True)
samples=[]
seed=0x12345678
for frame in range(8192):
    for channel in range(2):
        seed=(seed*1664525+1013904223)&0xffffffff
        noise=((seed>>16)-32768)/32768
        phase=2*math.pi*(80*frame/32768+14000*(frame/32768)**2/0.5)+channel*0.7
        samples.append(0.2*math.sin(phase)+0.025*noise)
raw=struct.pack('<'+str(len(samples))+'f',*samples)
(WORK/'source.f32le').write_bytes(raw)
subprocess.run([str(WORK/'harness'),str(WORK/'source.f32le'),str(WORK),'8192'],check=True)
if '--golden' in sys.argv:
    fixtures=ROOT/'crates/slot/tests/fixtures/audio'
    fixtures.mkdir(parents=True,exist_ok=True)
    subprocess.run([str(WORK/'harness'),str(WORK/'source.f32le'),str(fixtures),'4096'],check=True)
print('C reference generation complete', flush=True)
