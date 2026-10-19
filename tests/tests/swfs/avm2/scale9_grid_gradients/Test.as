package {
    import flash.display.BitmapData;
    import flash.display.GradientType;
    import flash.display.Graphics;
    import flash.display.Shape;
    import flash.display.Sprite;
    import flash.geom.Matrix;
    import flash.geom.Rectangle;

    public class Test extends Sprite {
        public function Test() {
            for each (var type:String in [GradientType.LINEAR, GradientType.RADIAL]) {
                for each (var shapeChild:Boolean in [false, true]) {
                    var sliced:BitmapData = drawPanel(type, shapeChild, true);
                    var ordinary:BitmapData = drawPanel(type, shapeChild, false);
                    var mismatches:int = 0;
                    for (var y:int = 0; y < 90; y++) {
                        for (var x:int = 0; x < 120; x++) {
                            var a:uint = sliced.getPixel(x, y);
                            var b:uint = ordinary.getPixel(x, y);
                            for (var shift:int = 0; shift < 24; shift += 8) {
                                if (Math.abs(((a >> shift) & 255) - ((b >> shift) & 255)) > 2) {
                                    mismatches++;
                                }
                            }
                        }
                    }
                    trace(type, shapeChild, mismatches);
                    sliced.dispose();
                    ordinary.dispose();
                }
            }
        }

        private function drawPanel(type:String, shapeChild:Boolean, grid:Boolean):BitmapData {
            var parent:Sprite = new Sprite();
            var child:Sprite = new Sprite();
            parent.addChild(child);
            var g:Graphics = child.graphics;
            if (shapeChild) {
                var shape:Shape = new Shape();
                child.addChild(shape);
                g = shape.graphics;
            }
            var m:Matrix = new Matrix();
            m.createGradientBox(60, 60, 0.3);
            g.beginGradientFill(type, [0xFF0000, 0x0000FF], [1, 1], [0, 255], m);
            g.drawRect(0, 0, 60, 60);
            g.endFill();
            if (grid) {
                child.scale9Grid = new Rectangle(10, 10, 40, 40);
            }
            child.scaleX = 2;
            child.scaleY = 1.5;
            var bitmap:BitmapData = new BitmapData(120, 90, false, 0xFFFFFF);
            bitmap.draw(parent);
            return bitmap;
        }
    }
}
