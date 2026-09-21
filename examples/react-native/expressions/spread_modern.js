function sum(...xs){return xs.reduce(function(s,x){return s+x;},0);} var a=[1,2,3]; print(sum(...a)); var b=[...a,4,5]; print(b.join(","));
