# JNA and the UniFFI-generated bindings are looked up reflectively.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class net.alummahnow.core.** { *; }
-dontwarn java.awt.**
