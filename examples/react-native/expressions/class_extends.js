class A{who(){return "A";}} class B extends A{who(){return "B+"+super.who();}} print(new B().who());
