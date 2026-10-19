The loaded `tone.swf` contains one timeline event sound: 4,410 mono PCM16
samples at 44,100 Hz, each with value 8,192, looped 1,000 times. It has no
ActionScript and uses SWF version 11. Its constant amplitude is 0.25.

The test distinguishes ordinary `unload()` from `unloadAndStop()`, checks that
stopping one loader leaves another loader's sound intact, and checks both
values of the optional GC argument and repeated calls on an empty loader.
