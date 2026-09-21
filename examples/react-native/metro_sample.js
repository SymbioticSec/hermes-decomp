// Minimal Metro-style bundle: module 0 requires module 1 and re-exports.
__d(function (global, require, module, exports, dependencyMap) {
  var dep = require(dependencyMap[0]);
  exports.greet = function (name) {
    return dep.prefix + name;
  };
  exports.dep = dep;
}, 0, [1]);
__d(function (global, require, module, exports, dependencyMap) {
  exports.prefix = "hi ";
}, 1, []);
__r(0);
