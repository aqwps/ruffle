package {
    import flash.display.Sprite;
    import flash.display.Loader;
    import flash.events.Event;
    import flash.media.SoundMixer;
    import flash.net.URLRequest;

    public class Test extends Sprite {
        private var first:Loader = new Loader();
        private var second:Loader = new Loader();
        private var frame:int = 0;

        public function Test() {
            first.load(new URLRequest("tone.swf"));
            addEventListener(Event.ENTER_FRAME, tick);
        }

        private function tick(event:Event):void {
            frame++;
            if (frame == 4) {
                trace("Ordinary unload keeps the event sound playing");
                first.unload();
            } else if (frame == 6) {
                SoundMixer.stopAll();
            } else if (frame == 7) {
                first.load(new URLRequest("tone.swf"));
                second.load(new URLRequest("tone.swf"));
            } else if (frame == 12) {
                trace("Stop the first loader, preserve the second loader's sound");
                first.unloadAndStop(false);
            } else if (frame == 15) {
                trace("Stop the second loader; no sounds remain");
                second.unloadAndStop();
                // Calling it again on an empty loader must be harmless.
                second.unloadAndStop();
            }
        }
    }
}
