using System;

namespace App.Services
{
    /// <summary>Users.</summary>
    [Serializable]
    public class UserService : IUserService
    {
        private readonly IRepo _repo;

        public UserService(IRepo repo)
        {
            _repo = repo;
            Init();
        }

        public string Name { get; set; }

        public async Task<User> FindAsync(int id)
        {
            var u = await _repo.GetAsync(id);
            if (u == null) throw new NotFoundException();
            return u;
        }
    }

    public interface IUserService
    {
        Task<User> FindAsync(int id);
    }

    public record Point(int X, int Y);
}
