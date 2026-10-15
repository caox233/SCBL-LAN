/* Test-only pure native provider. Four independently linked x86 DLLs exercise
 * the actual host LoadLibrary/GetProcAddress/table-publication code. */
#include <stdint.h>
typedef struct { uint32_t viewer, target, bms, intel; } NameInput;
typedef struct { float native_range; uint32_t shoes; } AtsInput;
typedef uint32_t (__stdcall *NameRule)(const NameInput*);
typedef float (__stdcall *AtsRule)(const AtsInput*);
typedef struct { uint32_t abi,size,capabilities,revision; NameRule name; AtsRule ats; } Api;
static uint32_t __stdcall name(const NameInput* x) {
    if (!x || x->bms>1 || x->intel>1) return 0;
    if (x->viewer==2 && x->target==1) return x->bms;
    if (x->viewer==1 && x->target==2) return x->intel;
    return 0;
}
static float __stdcall ats(const AtsInput* x) {
    if (!x) return 0;
#ifdef BAD_OUTPUT
    return x->native_range+1;
#else
    return x->shoes==1 ? x->native_range/ATS_DIVISOR : x->native_range;
#endif
}
int __stdcall scbl_mod_rules_get_api(uint32_t abi, Api* out, uint32_t size) {
    if (abi!=1 || !out || size!=sizeof(Api)) return -1;
    out->abi=PROVIDER_ABI; out->size=sizeof(Api);out->capabilities=3;
    out->revision=PROVIDER_REV;out->name=name;out->ats=ats;return 0;
}
