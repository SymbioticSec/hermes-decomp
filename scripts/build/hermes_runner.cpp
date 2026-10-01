// Standalone Hermes VM runner. This is not part of the Rust crate. It is a
// toolchain helper, like the prebuilt hermesc and hermes binaries. It links the
// hermesvm framework from a hermes ios Maven artifact and executes a .hbc, so
// write path output can be verified on a real modern engine (HBC 97 or newer).
// Built by build_hermes_v98_toolchain.sh.
#include <hermes/hermes.h>
#include <jsi/jsi.h>
#include <fstream>
#include <iostream>
#include <iterator>
using namespace facebook;
int main(int argc, char** argv) {
  if (argc < 2) { std::cerr << "usage: hermes-run file.hbc\n"; return 2; }
  std::ifstream f(argv[1], std::ios::binary);
  std::string bytes((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
  auto rt = facebook::hermes::makeHermesRuntime();
  auto printFn = jsi::Function::createFromHostFunction(
    *rt, jsi::PropNameID::forAscii(*rt, "print"), 1,
    [](jsi::Runtime& rt, const jsi::Value&, const jsi::Value* args, size_t n) -> jsi::Value {
      for (size_t i = 0; i < n; i++) { std::cout << args[i].toString(rt).utf8(rt); if (i+1<n) std::cout << " "; }
      std::cout << std::endl; return jsi::Value::undefined();
    });
  rt->global().setProperty(*rt, "print", printFn);
  try {
    auto buf = std::make_shared<jsi::StringBuffer>(bytes);
    rt->evaluateJavaScript(buf, argv[1]);
  } catch (const std::exception& e) { std::cerr << "Uncaught: " << e.what() << std::endl; return 1; }
  return 0;
}
