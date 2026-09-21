async function f(x){ var a = await Promise.resolve(x); var b = await Promise.resolve(a+1); return a+b; } f(10).then(r=>print(r));
