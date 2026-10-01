function* gen(){yield 1;yield 2;yield 3;} var g=gen(),out=[]; var r=g.next(); while(!r.done){out.push(r.value);r=g.next();} print(out.join(","));
