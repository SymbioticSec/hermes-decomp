async function sum(arr){ var t=0; for (var i=0;i<arr.length;i++){ t += await Promise.resolve(arr[i]); } return t; } sum([1,2,3]).then(r=>print(r));
