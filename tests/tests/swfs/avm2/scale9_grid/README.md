This visual test uses nine solid rectangles with ten-pixel outer regions. The
expected images are analytical fixtures, not Flash Player captures. For a
120-by-90 panel the color boundaries are x = [0, 10, 110, 120] and
y = [0, 10, 80, 90]. After changing the grid to (5, 5, 50, 50), they are
x = [0, 16, 104, 120] and y = [0, 13, 77, 90]. Clearing the grid restores
ordinary scaling: x = [0, 20, 100, 120], y = [0, 15, 75, 90].

Rows, from left to right:

1. Sprite artwork; a cached child Shape; horizontal reflection.
2. A cached Sprite; artwork clipped by a mask; rotation disabling the grid.
3. Shrinking below the minimum width; scaling the parent; a child Sprite which
   must keep its ordinary transform.

The second and third captures exercise changing and clearing the grid without
changing the cached object's dimensions or transform. The trace also checks
that BitmapData.draw ignores the source's own transform.

The companion scale9_grid_gradients test compares linear and radial gradient
fills with ordinary scaling, both on Sprite artwork and on a child Shape.

Current implementation limitation: artwork used to construct a stencil mask
still uses ordinary scaling, because nested stencil-mask construction cannot
express the per-slice intersection. Artwork clipped *by* a mask is supported.
