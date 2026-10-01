// Minimal Metro-style bundle: module 0 requires module 1 and re-exports.
__d(function (global, require, module, exports, dependencyMap) {
  // The dependency is named by its constant id; the dependency map stays in
  // the signature so the factory keeps Metro's shape.
  var dep = require(1);
  exports.greet = function (name) {
    return dep.prefix + name;
  };
  exports.dep = dep;
}, 0, [1]);
__d(function (global, require, module, exports, dependencyMap) {
  exports.prefix = "hi ";
}, 1, []);
__r(0);
