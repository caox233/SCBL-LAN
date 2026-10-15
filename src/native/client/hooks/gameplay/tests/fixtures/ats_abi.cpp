// Independent MSVC x86 member-function ABI fixture. Not shipped to the game.
#include <windows.h>
struct Entry { unsigned words[11]; };
class Fixture {
public:
    unsigned tag, calls;
    __declspec(noinline) void range(Entry e, float* distance, float* scale, unsigned* kind) {
        ++calls;
        *distance = 2000.0f + float(e.words[0]);
        *scale = 7.25f + float(e.words[10]);
        *kind = 77 + e.words[9];
    }
};
typedef void (Fixture::*Method)(Entry,float*,float*,unsigned*);
static_assert(sizeof(Method)==4,"single-inheritance x86 fixture only");
union Address { Method method; unsigned address; };
extern "C" unsigned __stdcall fixture_entry() { Address a; a.method=&Fixture::range; return a.address; }
extern "C" void __stdcall fixture_invoke(unsigned address, Fixture* self, Entry e, float* distance, float* scale, unsigned* kind) {
    Address a; a.address=address; (self->*a.method)(e,distance,scale,kind);
}
