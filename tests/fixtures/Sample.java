package com.example;

/** A service. */
@Service
public class UserService extends Base {
    private final Repo repo;

    public UserService(Repo repo) {
        this.repo = repo;
        init();
        log();
    }

    /** Find a user. */
    public User find(long id) {
        User u = repo.get(id);
        if (u == null) throw new NotFound();
        return u;
    }

    interface Listener {
        void onEvent(String e);
    }

    enum State { ON, OFF }
}

record Point(int x, int y) {}
