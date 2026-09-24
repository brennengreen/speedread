<?php
namespace App\Http;

/** Controller. */
class UserController extends Controller
{
    public function __construct(private Repo $repo)
    {
        parent::__construct();
        $this->init();
        $this->log();
    }

    public function show(int $id): Response
    {
        $u = $this->repo->find($id);
        $v = $u->view();
        return response($v);
    }
}

function helper($x) {
    $y = $x + 1;
    $z = $y * 2;
    return $z;
}
