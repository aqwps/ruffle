package {
    import flash.display.BitmapData;
    import flash.display.DisplayObject;
    import flash.display.Graphics;
    import flash.display.Shape;
    import flash.display.Sprite;
    import flash.events.Event;
    import flash.geom.Rectangle;

    [SWF(width="900", height="400", frameRate="1", backgroundColor="#FFFFFF")]
    public class Test extends Sprite {
        private var cached:Sprite;
        private var cachedChild:Sprite;
        private var frame:int = 0;

        public function Test() {
            // BitmapData.draw ignores the source's own scale, including its grid.
            var source:Sprite = panel(false);
            source.scaleX = 2;
            source.scaleY = 1.5;
            var bitmap:BitmapData = new BitmapData(60, 60, false);
            bitmap.draw(source);
            trace("untransformed", bitmap.getPixel(9, 9).toString(16), bitmap.getPixel(10, 10).toString(16));
            bitmap.dispose();
            source = panel(true);
            source.scaleX = 2;
            source.scaleY = 1.5;
            bitmap = new BitmapData(60, 60, false);
            bitmap.draw(source.getChildAt(0));
            trace("untransformed shape", bitmap.getPixel(9, 9).toString(16), bitmap.getPixel(10, 10).toString(16));
            bitmap.dispose();

            var own:Sprite = panel(false);
            place(own, 20, 20);

            // A timeline-style Shape belongs to its containing sprite's grid.
            cachedChild = panel(true);
            cachedChild.getChildAt(0).cacheAsBitmap = true;
            place(cachedChild, 320, 20);

            var reflected:Sprite = panel(false);
            place(reflected, 860, 20);
            reflected.scaleX = -2;

            cached = panel(false);
            cached.cacheAsBitmap = true;
            place(cached, 20, 140);

            var clipped:Sprite = panel(true);
            place(clipped, 320, 140);
            var clip:Shape = new Shape();
            clip.graphics.beginFill(0);
            clip.graphics.drawRect(320, 140, 65, 90);
            addChild(clip);
            clipped.mask = clip;

            // Rotation of the grid owner disables nine-slice scaling.
            var rotated:Sprite = panel(false);
            place(rotated, 860, 140);
            rotated.rotation = 90;

            var small:Sprite = panel(false);
            place(small, 20, 280);
            small.scaleX = 0.2;

            // Parent scaling still scales the preserved corners.
            var parent:Sprite = new Sprite();
            addChild(parent);
            parent.x = 320;
            parent.y = 280;
            parent.scaleX = 1.5;
            var child:Sprite = panel(false);
            parent.addChild(child);
            child.scaleX = 2;

            // A nested Sprite keeps the ordinary transform of its parent.
            var nested:Sprite = panel(false);
            place(nested, 620, 280);
            var marker:Sprite = new Sprite();
            marker.graphics.beginFill(0x000000);
            marker.graphics.drawRect(0, 0, 6, 6);
            marker.x = 25;
            marker.y = 20;
            nested.addChild(marker);

            addEventListener(Event.ENTER_FRAME, onFrame);
        }

        private function onFrame(event:Event):void {
            if (++frame == 2) {
                // Change the grid without changing the cache size or matrix.
                cached.scale9Grid = new Rectangle(5, 5, 50, 50);
                cachedChild.scale9Grid = new Rectangle(5, 5, 50, 50);
            } else if (frame == 3) {
                cached.scale9Grid = null;
                cachedChild.scale9Grid = null;
            }
        }

        private function place(object:DisplayObject, x:Number, y:Number):void {
            addChild(object);
            object.x = x;
            object.y = y;
            object.scaleX = 2;
            object.scaleY = 1.5;
        }

        private function panel(shapeChild:Boolean):Sprite {
            var sprite:Sprite = new Sprite();
            var g:Graphics = sprite.graphics;
            if (shapeChild) {
                var shape:Shape = new Shape();
                sprite.addChild(shape);
                g = shape.graphics;
            }
            var positions:Array = [0, 10, 50, 60];
            var colors:Array = [0xFF0000, 0x00AA00, 0x0000FF,
                                0xFFFF00, 0x888888, 0x00FFFF,
                                0xAA00AA, 0x884400, 0x000000];
            for (var row:int = 0; row < 3; row++) {
                for (var col:int = 0; col < 3; col++) {
                    g.beginFill(colors[row * 3 + col]);
                    g.drawRect(positions[col], positions[row],
                        positions[col + 1] - positions[col],
                        positions[row + 1] - positions[row]);
                    g.endFill();
                }
            }
            sprite.scale9Grid = new Rectangle(10, 10, 40, 40);
            return sprite;
        }
    }
}
