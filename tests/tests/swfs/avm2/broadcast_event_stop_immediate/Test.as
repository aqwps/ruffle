package {
    import flash.display.Sprite;
    import flash.events.Event;
    import flash.text.TextField;
    import flash.text.TextFormat;

    [SWF(width="500", height="500", frameRate="1", backgroundColor="#FFFFFF")]
    public class Test extends Sprite {
        // Broadcast events must also reach objects outside the display list.
        private var first:Sprite = new Sprite();
        private var second:Sprite = new Sprite();
        private var counts:Object = {};
        private var output:TextField = new TextField();

        public function Test() {
            output.defaultTextFormat = new TextFormat("_sans", 16, 0);
            output.width = 500;
            output.height = 500;
            addChild(output);
            var types:Array = [Event.ENTER_FRAME, Event.FRAME_CONSTRUCTED,
                Event.EXIT_FRAME];
            for each (var type:String in types) {
                first.addEventListener(type, stopFirst);
                first.addEventListener(type, shouldNotRun, false, -1);
                second.addEventListener(type, receiveSecond);
            }
        }

        private function write(type:String, target:String):void {
            var key:String = type + ":" + target;
            counts[key] = int(counts[key]) + 1;
            // Keep the reference player's on-screen output bounded as well.
            if (counts[key] <= 2) {
                trace(type, target);
                output.appendText(type + " " + target + "\n");
            }
        }

        private function stopFirst(event:Event):void {
            write(event.type, "first");
            event.stopImmediatePropagation();
        }

        private function shouldNotRun(event:Event):void {
            trace("ERROR: stopped listener ran", event.type);
        }

        private function receiveSecond(event:Event):void {
            write(event.type, "second");
        }
    }
}
