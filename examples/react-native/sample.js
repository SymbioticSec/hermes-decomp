// Canonical "app" sample for the per-version corpus. Exercises a broad spread of
// language features and prints a deterministic trace so the decompiled output can
// be run and diffed against the original (A->Z round-trip). No Date/random.

function add(a, b) {
  return a + b;
}

var obj = { a: 1, b: 2, label: "obj" };

function makeCounter() {
  var count = 0;
  return function () {
    count += 1;
    return count;
  };
}

function classify(n) {
  switch (true) {
    case n < 0:
      return "neg";
    case n === 0:
      return "zero";
    default:
      return "pos";
  }
}

function sumArray(arr) {
  var total = 0;
  for (var i = 0; i < arr.length; i++) {
    total += arr[i];
  }
  return total;
}

function safeDivide(a, b) {
  try {
    if (b === 0) throw new Error("div by zero");
    return a / b;
  } catch (e) {
    return e.message;
  }
}

function Greeter(name) {
  this.name = name;
}
Greeter.prototype.greet = function () {
  return "hello " + this.name;
};

function run() {
  var counter = makeCounter();
  var nums = [3, 1, 4, 1, 5, 9];
  var doubled = nums.map(function (x) {
    return x * 2;
  });
  var evens = nums.filter(function (x) {
    return x % 2 === 0;
  });

  print("add: " + add(obj.a, obj.b));
  print("counter: " + counter() + "," + counter());
  print("classify: " + classify(-2) + "," + classify(0) + "," + classify(7));
  print("sum: " + sumArray(nums));
  print("doubled: " + doubled.join(","));
  print("evens: " + evens.join(","));
  print("divide: " + safeDivide(10, 2) + "," + safeDivide(1, 0));
  print("greet: " + new Greeter("world").greet());
  print("ternary: " + (nums.length > 3 ? "many" : "few"));
}

run();
