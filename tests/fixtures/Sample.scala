package app

trait Shape {
  def area: Double
}

case class Circle(r: Double) extends Shape {
  def area: Double = {
    val a = math.Pi
    val b = r * r
    a * b
  }
}

object Main {
  def main(args: Array[String]): Unit = {
    val c = Circle(1.0)
    println(c.area)
    println("done")
  }
}
