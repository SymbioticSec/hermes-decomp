function sum(){var t=0;for(var i=0;i<arguments.length;i++)t+=arguments[i];return t;} var a=[1,2,3]; print(sum.apply(null,a)); var b=[0].concat(a); print(b.join(","));
