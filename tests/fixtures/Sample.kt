package app

/** A repository. */
class UserRepo(private val db: Db) : Repo {
    fun find(id: Long): User? {
        val row = db.query(id)
        val user = row?.toUser()
        return user
    }

    companion object {
        fun create(): UserRepo {
            val db = Db()
            db.open()
            return UserRepo(db)
        }
    }
}

interface Repo {
    fun find(id: Long): User?
}

object Registry {
    val items = mutableListOf<String>()
}

fun main() {
    val r = UserRepo.create()
    println(r.find(1))
    println("done")
}
