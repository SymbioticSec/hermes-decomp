function safe(x){try{if(x<0)throw new Error("neg");return x*2;}catch(e){return e.message;}finally{}} print(safe(3)); print(safe(-1));
