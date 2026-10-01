function* g(){ var a = yield 1; var b = yield a+1; return a+b; } var it=g(); var r1=it.next(); var r2=it.next(10); var r3=it.next(20); print(r1.value, r2.value, r3.value);
