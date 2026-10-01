function defineProps(target, props) {
  for (var i = 0; i < props.length; i++) {
    var d = props[i];
    d.enumerable = d.enumerable || false;
    d.configurable = true;
    if ("value" in d) d.writable = true;
    Object.defineProperty(target, d.key, d);
  }
}
var o = {};
defineProps(o, [{ key: "a", value: 10 }, { key: "b", value: 20 }, { key: "c", value: 30 }]);
print(o.a);
print(o.b);
print(o.c);
