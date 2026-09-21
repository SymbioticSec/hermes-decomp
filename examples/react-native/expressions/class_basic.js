function Animal(name){this.name=name;} Animal.prototype.speak=function(){return this.name+" makes a sound";}; var a=new Animal("dog"); print(a.speak());
