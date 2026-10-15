#include <windows.h>
#include <stdint.h>
#include <math.h>
static void* bridge;
static uint32_t allow, errors, frame_before, callbacks;
__declspec(align(16)) static unsigned char before_fx[512],after_fx[512],caller_fx[512];
static float one=1.25f;
extern "C" void __stdcall rfd_callback(uint32_t frame) {
 if(frame!=frame_before) errors|=1;
 ++callbacks;
 *(unsigned char*)(frame-0x29)=(unsigned char)allow;
 __asm { fninit
         pxor xmm0,xmm0
         pxor xmm1,xmm1
         stc }
}
extern "C" void __stdcall validate(uint32_t* saved) {
 // pushad: EDI ESI EBP ESP EBX EDX ECX EAX; following EFLAGS.
 if(saved[0]!=0x77777777 || saved[1]!=0x66666666 ||
    saved[2]!=frame_before || saved[4]!=0x44444444 ||
    saved[5]!=0x33333333 || saved[6]!=0x22222222 || saved[7]!=allow) errors|=2;
 if(((saved[8]>>6)&1)!=(allow==0)) errors|=4;
 // Compare control/status/tag and x87/MMX + all XMM. FIP/FDP can vary in hardware.
 for(int i=0;i<8;++i) if(before_fx[i]!=after_fx[i]) errors|=8;
 for(int i=24;i<288;++i) if(before_fx[i]!=after_fx[i]) errors|=8;
}
extern "C" __declspec(naked) void rfd_resume() {
 __asm {
   pushfd
   pushad
   fxsave after_fx
   mov eax,esp
   push eax
   call validate
   popad
   popfd
   pop edi
   pop esi
   pop ebx
   mov esp,ebp
   pop ebp
   ret
 }
}
__declspec(naked) void probe() {
 __asm {
   push ebp
   mov ebp,esp
   sub esp,210h
   push ebx
   push esi
   push edi
   mov frame_before,ebp
   mov byte ptr[ebp-29h],1
   fninit
   fld one
   movss xmm0,one
   movss xmm1,one
   fxsave before_fx
   mov eax,11111111h
   mov ecx,22222222h
   mov edx,33333333h
   mov ebx,44444444h
   mov esi,66666666h
   mov edi,77777777h
   clc
   jmp bridge
 }
}
extern "C" __declspec(dllexport) uint32_t __stdcall fixture_run(void* p,uint32_t a) {
 bridge=p;allow=a;errors=0;callbacks=0;
 __asm { fxsave caller_fx }
 for(int i=0;i<1000;++i) probe();
 __asm { fxrstor caller_fx }
 if(callbacks!=1000) errors|=16;
 return errors;
}
extern "C" __declspec(dllexport) void* __stdcall fixture_callback(){return (void*)&rfd_callback;}
extern "C" __declspec(dllexport) void* __stdcall fixture_resume(){return (void*)&rfd_resume;}
struct Weapon {
 float multiplier;
 __declspec(noinline) float __thiscall range(void* target,uint8_t context) {
   if(target!=(void*)0x12345678) return -1.0f;
   return (context?120.0f:180.0f)*multiplier;
 }
};
extern "C" __declspec(dllexport) void* __stdcall fixture_melee() {
 float (__thiscall Weapon::*f)(void*,uint8_t)=&Weapon::range;
 return *(void**)&f;
}
extern "C" __declspec(dllexport) float __stdcall fixture_invoke(void* p,uint8_t context) {
 return ((Weapon*)p)->range((void*)0x12345678,context);
}
