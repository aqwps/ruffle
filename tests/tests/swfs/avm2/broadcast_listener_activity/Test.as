package {
    import flash.display.Sprite;
    import flash.events.Event;

    public class Test extends Sprite {
        private var a:Sprite = new Sprite();
        private var b:Sprite = new Sprite();
        private var c:Sprite = new Sprite();
        private var d:Sprite = new Sprite();
        private var e:Sprite = new Sprite();
        private var frame:int = 0;

        public function Test() {
            a.removeEventListener(Event.ENTER_FRAME, onA);
            addEventListener(Event.ENTER_FRAME, tick);
            a.addEventListener(Event.ENTER_FRAME, onA);
            b.addEventListener(Event.ENTER_FRAME, onB);
            c.addEventListener(Event.ENTER_FRAME, captureOnly, true);
            d.addEventListener(Event.ENTER_FRAME, onD);
            d.removeEventListener(Event.ENTER_FRAME, onD);
        }

        private function tick(event:Event):void {
            frame++;
            trace("frame", frame);
            if (frame == 2) {
                a.removeEventListener(Event.ENTER_FRAME, onA);
                b.addEventListener(Event.ENTER_FRAME, onB);
            } else if (frame == 3) {
                a.addEventListener(Event.ENTER_FRAME, onA);
                b.removeEventListener(Event.ENTER_FRAME, onB);
                b.addEventListener(Event.ENTER_FRAME, captureOnly, true);
            } else if (frame == 4) {
                b.addEventListener(Event.ENTER_FRAME, onB);
            }
        }

        private function onA(event:Event):void {
            trace("A");
            if (frame == 1) {
                b.removeEventListener(Event.ENTER_FRAME, onB);
                c.addEventListener(Event.ENTER_FRAME, onC);
                d.addEventListener(Event.ENTER_FRAME, onD);
                e.addEventListener(Event.ENTER_FRAME, onE);
            }
        }
        private function onB(event:Event):void { trace("B"); }
        private function onC(event:Event):void {
            trace("C");
            if (frame == 2) {
                d.removeEventListener(Event.ENTER_FRAME, onD);
                c.removeEventListener(Event.ENTER_FRAME, onC);
            }
        }
        private function onD(event:Event):void { trace("D"); }
        private function onE(event:Event):void { trace("E"); }
        private function captureOnly(event:Event):void { trace("ERROR: capture listener ran"); }
    }
}
