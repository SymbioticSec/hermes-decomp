var a=[1,2,3,4]; print(a.map(function(x){return x*2;}).join(",")); print(a.filter(function(x){return x%2===0;}).join(",")); print(a.reduce(function(s,x){return s+x;},0));
